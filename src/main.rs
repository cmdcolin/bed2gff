//! # bed2gff
//! A Rust BED-to-gff translator.
//!
//! ## Overview
//! `bed2gff` is a Rust-based utility designed to facilitate
//! the conversion of BED files to gff files. This tool offers
//! good performance and quick results, making it, filling a
//! gap in the current landscape of BED-to-GTF converters.
//! The main objective of `bed2gff` is to streamline the process of
//! translating genomic data from the BED format to the gff format,
//! enabling easier downstream analysis.
//!
//!
//! ## Usage
//!
//! ### Installation
//!
//! `bed2gff` can be easily installed and used on your system.
//! Detailed installation instructions are available
//! on the [GitHub repository](https://github.com/alejandrogzi/bed2gff).
//!
//! ### Conversion
//!
//! To convert a BED file to a gff file using `bed2gff`, you can use the
//! following command:
//!
//! ```shell
//! bed2gff -b input.bed -i isoforms.txt -o output.gff
//! ```
//!
//! Where:
//! - `input.bed` is the input BED file you want to convert.
//! - `isoforms.txt` is a file that contains information about isoforms.
//! - `output.gff3` is the output gff file where the conversion results
//! will be stored.
//!
//! ## Output
//!
//! `bed2gff` produces gff files compliant with the GTF3 standard.
//! The resulting GFF file contains detailed annotations of genomic
//! features, including genes, transcripts, exons, coding
//! sequences (CDS), start codons, and stop codons.
//!
//! ## Contact and Support
//!
//! For inquiries, bug reports, or suggestions, please
//! visit the [GitHub repository](https://github.com/alejandrogzi/bed2gff).
//! We welcome your feedback and contributions to enhance this tool.

use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::string::String;
use std::time::Instant;

use clap::{self, Parser};
use flate2::write::GzEncoder;
use flate2::Compression;
use log::Level;
use natord::compare;
use rayon::prelude::*;

use bed2gff::*;

const SOURCE: &str = "bed2gff";

fn main() {
    let args = Cli::parse();
    args.check().unwrap_or_else(|e| {
        log::error!("{}", e);
        std::process::exit(1);
    });

    simple_logger::init_with_level(Level::Info).unwrap();

    rayon::ThreadPoolBuilder::new()
        .num_threads(args.threads)
        .build_global()
        .unwrap();


    let start = Instant::now();
    let bmem = max_mem_usage_mb();

    let imap = if !args.no_gene {
        let isf = reader(&args.isoforms.unwrap()).unwrap_or_else(|_| {
            let message = format!("Error reading isoforms file",);
            panic!("{}", message);
        });
        get_isoforms(&isf)
    } else {
        HashMap::new()
    };

    let bed = bed_reader(&args.bed);
    let gene_track = custom_par_parse(&bed).unwrap_or_else(|_| {
        let message = format!("Error parsing BED file {}", args.bed.display());
        panic!("{}", message);
    });

    let results = bed
        .par_iter()
        .filter_map(|record| to_gff(record, &imap).ok())
        .flatten()
        .collect::<Vec<_>>();

    let mut blocks = combine_maps_par(&imap, &gene_track);
    blocks.extend(results);

    blocks.par_sort_unstable_by(|a, b| {
        let chr_cmp = compare(&a.0, &b.0);
        if chr_cmp == std::cmp::Ordering::Equal {
            a.2.cmp(&b.2)
        } else {
            chr_cmp
        }
    });

    let writer_boxed: Box<dyn Write> = if args.gz {
        let file = File::create(&args.output).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        Box::new(BufWriter::new(encoder))
    } else {
        let file = File::create(&args.output).unwrap();
        Box::new(BufWriter::new(file))
    };

    let mut writer = writer_boxed;
    comments(&mut writer);

    for entry in &blocks {
        writeln!(
            writer,
            "{}\t{}\t{}\t{}\t{}\t.\t{}\t{}\t{}",
            entry.0, SOURCE, entry.1, entry.2, entry.3, entry.4, entry.5, entry.6
        )
        .unwrap();
    }

    let peak_mem = (max_mem_usage_mb() - bmem).max(0.0);
}

fn to_gff(
    bedline: &BedRecord,
    isoforms: &HashMap<String, String>,
) -> Result<Vec<(String, String, u32, u32, String, String, String)>, Box<dyn Error>> {
    let mut result: Vec<(String, String, u32, u32, String, String, String)> = Vec::new();

    let gene = if !isoforms.is_empty() {
        match isoforms.get(&bedline.name) {
            Some(g) => g,
            None => {
                log::error!("Gene {} not found in isoforms file.", bedline.name);
                std::process::exit(1)
            }
        }
    } else {
        &bedline.name
    };

    let fcodon = first_codon(bedline)
        .unwrap_or_else(|| panic!("No start codon found for {}.", bedline.name));
    let lcodon = last_codon(bedline).unwrap_or_else(|| {
        panic!("No stop codon found for {}.", bedline.name);
    });
    // let first_utr_end = bedline.cds_start;
    // let last_utr_start = bedline.cds_end;
    let frames = bedline.get_frames();

    let cds_end: u32 = if bedline.strand == "+" && codon_complete(&lcodon) {
        move_pos(bedline, lcodon.end, -3)
    } else {
        bedline.cds_end
    };

    let cds_start = if bedline.strand == "-" && codon_complete(&fcodon) {
        move_pos(bedline, fcodon.start, 3)
    } else {
        bedline.cds_start
    };

    build_gff_line(
        bedline,
        gene,
        "transcript",
        bedline.tx_start,
        bedline.tx_end,
        3,
        -1,
        &mut result,
    );

    for i in 0..bedline.exon_count as usize {
        build_gff_line(
            bedline,
            gene,
            "exon",
            bedline.exon_start[i],
            bedline.exon_end[i],
            3,
            i as i16,
            &mut result,
        );
        if cds_start < cds_end {
            write_features(
                i,
                bedline,
                gene,
                // first_utr_end,
                cds_start,
                cds_end,
                // last_utr_start,
                frames[i] as u32,
                &mut result,
            );
        }
    }

    if bedline.strand != "-" {
        if codon_complete(&fcodon) {
            write_codon(bedline, gene, "start_codon", fcodon, &mut result);
        }
        if codon_complete(&lcodon) {
            write_codon(bedline, gene, "stop_codon", lcodon, &mut result);
        }
    } else {
        if codon_complete(&lcodon) {
            write_codon(bedline, gene, "start_codon", lcodon, &mut result);
        }
        if codon_complete(&fcodon) {
            write_codon(bedline, gene, "stop_codon", fcodon, &mut result);
        }
    }

    Ok(result)
}

fn move_pos(record: &BedRecord, pos: u32, dist: i32) -> u32 {
    let mut pos = pos;
    assert!(record.tx_start <= pos && pos <= record.tx_end);

    let mut exon_index = record
        .exon_start
        .iter()
        .zip(record.exon_end.iter())
        .position(|(start, end)| pos >= *start && pos <= *end)
        .unwrap_or_else(|| {
            let message = format!("Position {} not in exons.", pos);
            panic!("{}", message);
        }) as i16;

    let mut steps = dist.abs();
    let direction = if dist >= 0 { 1 } else { -1 };

    while steps > 0 {
        let (exon_start, exon_end) = (
            record.exon_start[exon_index as usize],
            record.exon_end[exon_index as usize],
        );

        if pos >= exon_start && pos <= exon_end {
            pos += direction as u32;
            steps -= 1;
        } else if direction >= 0 {
            exon_index += 1;
            if (exon_index as usize) < record.exon_count as usize {
                pos = record.exon_start[exon_index as usize];
            }
        } else {
            exon_index -= 1;
            if exon_index >= 0 {
                pos = record.exon_end[exon_index as usize] - 1;
                steps -= 1;
            }
        }
    }
    if steps > 0 {
        panic!("can't move {} by {}", pos, dist);
    }
    pos
}
