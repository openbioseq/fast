//! Deterministic record-level downsampling.
//!
//! For seekable inputs this runs in two passes: the first pass counts records,
//! and the second keeps exactly `round(total * proportion)` records.  The
//! selection uses a stable hash of the record name (the first whitespace-
//! delimited token of the header) and Algorithm S, so the result is
//! reproducible, independent of the thread count, and preserves input order.
//!
//! stdin cannot be read twice.  When any input is `-`, sampling falls back to
//! a single-pass hash threshold; the selected fraction is then statistically
//! close to the requested proportion rather than exact.

use std::io::Write;

use anyhow::{Context, Result, bail};
use memchr::memmem;
use rayon::prelude::*;

use crate::parser::{
    Format, StreamConfig, fastq_record_end_at, plan_chunks, stream_path, stream_path_map,
};

#[derive(Clone, Copy, Debug)]
struct RecordSpan {
    hash: u64,
    start: usize,
    end: usize,
}

/// Sample all inputs at `proportion` using `seed`.
pub fn run(
    files: &[String],
    proportion: f64,
    seed: u64,
    cfg: &StreamConfig,
    out: &mut (dyn Write + Send),
) -> Result<()> {
    if files.iter().any(|path| path == "-") {
        return sample_threshold(files, proportion, seed, cfg, out);
    }

    let total = count_inputs(files, cfg)?;
    let target = target_count(total, proportion);

    if target == 0 {
        return Ok(());
    }

    let mut remaining = total;
    let mut needed = target;
    let mut out_state = OutputState::default();

    for path in files {
        sample_path(
            path,
            seed,
            cfg,
            &mut remaining,
            &mut needed,
            &mut out_state,
            out,
        )
        .with_context(|| path.clone())?;
    }

    if remaining != 0 || needed != 0 {
        bail!(
            "input changed between the counting and sampling passes ({} records left, {} still to select)",
            remaining,
            needed
        );
    }

    Ok(())
}

fn target_count(total: u64, proportion: f64) -> u64 {
    if proportion <= 0.0 {
        0
    } else if proportion >= 1.0 {
        total
    } else {
        ((total as f64) * proportion)
            .round()
            .clamp(0.0, total as f64) as u64
    }
}

fn count_inputs(files: &[String], cfg: &StreamConfig) -> Result<u64> {
    let mut total = 0u64;
    let mut common_format: Option<Format> = None;

    for path in files {
        let (count, format) = count_path(path, cfg).with_context(|| path.clone())?;
        total = total
            .checked_add(count)
            .context("record count overflowed u64")?;

        if let Some(format) = format {
            match common_format {
                Some(common) if common != format => {
                    bail!("mixed FASTA and FASTQ inputs cannot be sampled together");
                }
                None => common_format = Some(format),
                _ => {}
            }
        }
    }

    Ok(total)
}

fn count_path(path: &str, cfg: &StreamConfig) -> Result<(u64, Option<Format>)> {
    let mut total = 0u64;
    let mut file_format: Option<Format> = None;

    let detected = stream_path(path, cfg, |body, format, eof| {
        match file_format {
            Some(have) if have != format => {
                bail!(
                    "input format changed mid-file ({} -> {})",
                    have.label(),
                    format.label()
                );
            }
            None => file_format = Some(format),
            _ => {}
        }

        total = total
            .checked_add(count_body(body, format, eof)?)
            .context("record count overflowed u64")?;
        Ok(true)
    })?;

    Ok((total, detected.and(file_format)))
}

fn count_body(body: &[u8], format: Format, eof: bool) -> Result<u64> {
    let chunks = plan_chunks(body, format);
    let counts: Vec<Result<u64>> = chunks
        .par_iter()
        .map(|&(start, end)| {
            let last = eof && end == body.len();
            count_chunk(&body[start..end], format, last)
                .with_context(|| format!("records in byte range {start}..{end}"))
        })
        .collect();

    let mut total = 0u64;
    for count in counts {
        total = total
            .checked_add(count?)
            .context("record count overflowed u64")?;
    }
    Ok(total)
}

fn count_chunk(data: &[u8], format: Format, final_chunk: bool) -> Result<u64> {
    if data.is_empty() {
        return Ok(0);
    }

    match format {
        Format::Fasta => Ok(1 + memmem::find_iter(data, b"\n>").count() as u64),
        Format::Fastq => count_fastq_chunk(data, final_chunk),
    }
}

fn count_fastq_chunk(data: &[u8], final_chunk: bool) -> Result<u64> {
    let mut pos = 0usize;
    let mut count = 0u64;

    while pos < data.len() {
        match fastq_record_end_at(data, pos) {
            Some(end) if end > pos => {
                pos = end;
                count += 1;
            }
            _ if final_chunk => {
                // The last record may omit its trailing newline.
                count += 1;
                break;
            }
            _ => bail!("incomplete FASTQ record at byte offset {pos}"),
        }
    }

    Ok(count)
}

fn sample_path(
    path: &str,
    seed: u64,
    cfg: &StreamConfig,
    remaining: &mut u64,
    needed: &mut u64,
    out_state: &mut OutputState,
    out: &mut (dyn Write + Send),
) -> Result<()> {
    let mut file_format: Option<Format> = None;

    stream_path(path, cfg, |body, format, eof| {
        match file_format {
            Some(have) if have != format => {
                bail!(
                    "input format changed mid-file ({} -> {})",
                    have.label(),
                    format.label()
                );
            }
            None => file_format = Some(format),
            _ => {}
        }

        let chunks = plan_chunks(body, format);
        let spans: Vec<Result<Vec<RecordSpan>>> = chunks
            .par_iter()
            .map(|&(start, end)| {
                let last = eof && end == body.len();
                collect_records(&body[start..end], format, last, seed, start)
                    .with_context(|| format!("records in byte range {start}..{end}"))
            })
            .collect();

        let mut selected = Vec::new();
        for chunk in spans {
            for record in chunk? {
                if *remaining == 0 {
                    bail!("input has more records than the counting pass found");
                }

                if keep_record(record.hash, *remaining, *needed) {
                    selected.extend_from_slice(&body[record.start..record.end]);
                    *needed -= 1;
                }
                *remaining -= 1;
            }
        }

        out_state.write(out, &selected)?;
        Ok(true)
    })?;

    Ok(())
}

fn collect_records(
    data: &[u8],
    format: Format,
    final_chunk: bool,
    seed: u64,
    offset: usize,
) -> Result<Vec<RecordSpan>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    match format {
        Format::Fasta => Ok(collect_fasta_records(data, seed, offset)),
        Format::Fastq => collect_fastq_records(data, final_chunk, seed, offset),
    }
}

fn collect_fasta_records(data: &[u8], seed: u64, offset: usize) -> Vec<RecordSpan> {
    let mut starts = Vec::new();
    starts.push(0usize);
    starts.extend(memmem::find_iter(data, b"\n>").map(|pos| pos + 1));

    let mut records = Vec::with_capacity(starts.len());
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(data.len());
        records.push(RecordSpan {
            hash: hash_header(&data[start + 1..], seed),
            start: start + offset,
            end: end + offset,
        });
    }
    records
}

fn collect_fastq_records(
    data: &[u8],
    final_chunk: bool,
    seed: u64,
    offset: usize,
) -> Result<Vec<RecordSpan>> {
    let mut records = Vec::new();
    let mut pos = 0usize;

    while pos < data.len() {
        let end = match fastq_record_end_at(data, pos) {
            Some(end) if end > pos => end,
            _ if final_chunk => data.len(),
            _ => bail!("incomplete FASTQ record at byte offset {pos}"),
        };

        records.push(RecordSpan {
            hash: hash_header(&data[pos + 1..], seed),
            start: pos + offset,
            end: end + offset,
        });
        pos = end;
    }

    Ok(records)
}

/// Hash the first whitespace-delimited token of a header.  `header` starts
/// just after the `>`/`@` marker and may include the rest of the line.
fn hash_header(header: &[u8], seed: u64) -> u64 {
    let end = header
        .iter()
        .position(|b| b.is_ascii_whitespace())
        .unwrap_or(header.len());
    hash_name(&header[..end], seed)
}

fn hash_name(name: &[u8], seed: u64) -> u64 {
    // FNV-1a for the bytes, then a SplitMix64 finalizer for good avalanche.
    let mut hash = seed ^ 0xcbf2_9ce4_8422_2325;
    for &byte in name {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }

    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    hash ^= hash >> 33;
    hash
}

/// Algorithm S: keep the current record with probability
/// `needed / remaining`, which selects exactly `needed` records overall.
fn keep_record(hash: u64, remaining: u64, needed: u64) -> bool {
    if needed == 0 {
        false
    } else if needed >= remaining {
        true
    } else {
        (u128::from(hash) * u128::from(remaining)) < (u128::from(needed) << 64)
    }
}

/// Single-pass fallback for stdin: keep records whose hash falls below the
/// requested proportion.
fn sample_threshold(
    files: &[String],
    proportion: f64,
    seed: u64,
    cfg: &StreamConfig,
    out: &mut (dyn Write + Send),
) -> Result<()> {
    let mut expected: Option<Format> = None;
    let mut out_state = OutputState::default();

    for path in files {
        let result = stream_path_map(
            path,
            cfg,
            |body, format, eof| -> Result<Vec<Vec<u8>>> {
                let chunks = plan_chunks(body, format);
                chunks
                    .par_iter()
                    .map(|&(start, end)| {
                        let last = eof && end == body.len();
                        filter_chunk(&body[start..end], format, last, seed, proportion)
                            .with_context(|| format!("records in byte range {start}..{end}"))
                    })
                    .collect()
            },
            |chunks| {
                let mut selected = Vec::new();
                for chunk in chunks {
                    selected.extend_from_slice(&chunk);
                }
                out_state.write(out, &selected)?;
                Ok(true)
            },
        );

        let detected = result.with_context(|| path.clone())?;
        if let Some(format) = detected {
            match expected {
                Some(want) if want != format => {
                    bail!("mixed FASTA and FASTQ inputs cannot be sampled together");
                }
                None => expected = Some(format),
                _ => {}
            }
        }
    }

    Ok(())
}

fn filter_chunk(
    data: &[u8],
    format: Format,
    final_chunk: bool,
    seed: u64,
    proportion: f64,
) -> Result<Vec<u8>> {
    let records = collect_records(data, format, final_chunk, seed, 0)?;
    let mut selected = Vec::new();

    for record in records {
        if keep_fraction(record.hash, proportion) {
            selected.extend_from_slice(&data[record.start..record.end]);
        }
    }

    Ok(selected)
}

fn keep_fraction(hash: u64, proportion: f64) -> bool {
    if proportion <= 0.0 {
        false
    } else if proportion >= 1.0 {
        true
    } else {
        u64::from((hash >> 32) as u32) < (proportion * 4_294_967_296.0) as u64
    }
}

#[derive(Default)]
struct OutputState {
    wrote_any: bool,
    last_byte: Option<u8>,
}

impl OutputState {
    fn write(&mut self, out: &mut (dyn Write + Send), bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }

        // Keep concatenated multi-file output valid when a selected record
        // from an earlier file did not end with a newline.
        if self.wrote_any && self.last_byte != Some(b'\n') {
            out.write_all(b"\n")?;
        }
        out.write_all(bytes)?;

        self.wrote_any = true;
        self.last_byte = bytes.last().copied();
        Ok(())
    }
}
