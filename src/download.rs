use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use crate::history;
use crate::types::{DownloadTask, HistoryEntry};

#[derive(Debug)]
enum Aria2cError {
    NotFound,
    PrepareInputFailed(anyhow::Error),
    LaunchFailed(anyhow::Error),
    NonZeroExit(std::process::ExitStatus),
}

impl std::fmt::Display for Aria2cError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Aria2cError::NotFound => write!(f, "aria2c not found"),
            Aria2cError::PrepareInputFailed(e) => write!(f, "failed to prepare aria2c input: {e}"),
            Aria2cError::LaunchFailed(e) => write!(f, "failed to start aria2c: {e}"),
            Aria2cError::NonZeroExit(status) => write!(f, "aria2c exited with status: {status}"),
        }
    }
}

impl std::error::Error for Aria2cError {}

/// Find the aria2c binary (bundled or from PATH).
pub fn find_aria2c() -> Option<String> {
    // Try bundled binary on Windows
    #[cfg(windows)]
    {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                let bundled = parent.join("aria2").join("aria2c.exe");
                if bundled.exists() {
                    return Some(bundled.to_string_lossy().to_string());
                }
            }
        }
    }

    // PATH lookup
    which_aria2c()
}

fn which_aria2c() -> Option<String> {
    which::which("aria2c")
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

fn resolve_aria2c_binary() -> std::result::Result<PathBuf, Aria2cError> {
    find_aria2c().map(PathBuf::from).ok_or(Aria2cError::NotFound)
}

fn build_aria2_input_file(tasks: &[DownloadTask], output_dir: &Path) -> std::result::Result<PathBuf, Aria2cError> {
    let tmp_dir = output_dir.join(".sjtu_canvas_tmp");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| Aria2cError::PrepareInputFailed(e.into()))?;

    let input_path = tmp_dir.join(format!(
        "aria2_{}.txt",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));

    let file = File::create(&input_path)
        .with_context(|| format!("Failed to write aria2c input file: {}", input_path.display()))
        .map_err(Aria2cError::PrepareInputFailed)?;
    let mut writer = BufWriter::new(file);

    for task in tasks {
        writeln!(writer, "{}", task.url).map_err(|e| Aria2cError::PrepareInputFailed(e.into()))?;
        writeln!(writer, "  out={}", task.filename)
            .map_err(|e| Aria2cError::PrepareInputFailed(e.into()))?;
        writeln!(writer, "  header=referer: https://v.sjtu.edu.cn")
            .map_err(|e| Aria2cError::PrepareInputFailed(e.into()))?;
    }
    writer
        .flush()
        .map_err(|e| Aria2cError::PrepareInputFailed(e.into()))?;

    Ok(input_path)
}

fn cleanup_aria2_temp(input_path: &Path) {
    let _ = std::fs::remove_file(input_path);
    if let Some(parent) = input_path.parent() {
        // only remove if empty
        let _ = std::fs::remove_dir(parent);
    }
}

fn run_aria2c(binary: &Path, input_path: &Path, output_dir: &Path) -> std::result::Result<(), Aria2cError> {
    let mut cmd = Command::new(binary);
    cmd.arg("-d")
        .arg(output_dir)
        .arg("-i")
        .arg(input_path)
        .arg("-x")
        .arg("16")
        .arg("--auto-file-renaming=false")
        .arg("--summary-interval=1");

    let status = cmd.status().map_err(|e| Aria2cError::LaunchFailed(e.into()))?;

    if status.success() {
        Ok(())
    } else {
        Err(Aria2cError::NonZeroExit(status))
    }
}

fn run_default_aria2c_download(tasks: &[DownloadTask], output_dir: &Path) -> std::result::Result<(), Aria2cError> {
    let binary = resolve_aria2c_binary()?;
    let input_path = build_aria2_input_file(tasks, output_dir)?;

    println!("\n{}", style("Downloader: aria2c (default)").green().bold());
    println!(
        "{} {}",
        style("Output directory:").cyan(),
        output_dir.display()
    );
    println!("{} {}", style("Total files:").cyan(), tasks.len());
    println!("{} This may take a while...\n", style("Info:").yellow());

    let res = run_aria2c(&binary, &input_path, output_dir);
    cleanup_aria2_temp(&input_path);
    res
}

/// Sanitize a filename for the filesystem.
pub fn sanitize_filename(name: &str) -> String {
    name.replace(
        |c: char| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'),
        "_",
    )
}

/// Generate preview of filenames that would be downloaded.
pub fn preview_tasks(tasks: &[DownloadTask]) -> String {
    tasks
        .iter()
        .enumerate()
        .map(|(i, t)| format!("{:3}. {}", i + 1, t.filename))
        .collect::<Vec<_>>()
        .join("\n")
}

#[allow(dead_code)]
/// Download using aria2c with an input file.
pub fn download_with_aria2c(tasks: &[DownloadTask], output_dir: &Path) -> Result<()> {
    run_default_aria2c_download(tasks, output_dir).map_err(|e| anyhow::anyhow!(e))
}

/// Fallback: download each file using reqwest streaming with progress bar.
pub async fn download_with_reqwest(
    client: &reqwest::Client,
    tasks: &[DownloadTask],
    output_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(output_dir).context("Failed to create output directory")?;

    let total = tasks.len();

    // Overall progress bar for all files
    let main_pb = ProgressBar::new(total as u64);
    main_pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} files ({eta})",
        )
        .unwrap()
        .progress_chars("█▓▒░  "),
    );

    for (i, task) in tasks.iter().enumerate() {
        main_pb.set_message(format!("Downloading {}", task.filename));
        main_pb.tick();

        println!(
            "\n[{}/{}] {}",
            i + 1,
            total,
            style(&task.filename).cyan().bold()
        );

        let resp = client
            .get(&task.url)
            .header("Referer", "https://v.sjtu.edu.cn")
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .with_context(|| format!("Failed to fetch {}", task.url))?;

        let total_size = resp.content_length().unwrap_or(0);

        // Progress bar for this file
        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::with_template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, ETA: {eta})")
                .unwrap()
                .progress_chars("█▓▒░  ")
        );

        let tmp_path = output_dir.join(format!("{}.part", task.filename));
        let final_path = output_dir.join(&task.filename);

        {
            let mut file = File::create(&tmp_path)
                .with_context(|| format!("Failed to create {}", task.filename))?;

            let mut stream = resp.bytes_stream();
            use futures::StreamExt;

            let mut downloaded: u64 = 0;
            let start_time = Instant::now();

            while let Some(chunk) = stream.next().await {
                let chunk = chunk.context("Failed to read chunk")?;
                file.write_all(&chunk)
                    .with_context(|| format!("Failed to write {}", task.filename))?;

                downloaded += chunk.len() as u64;
                pb.set_position(downloaded);
            }

            pb.finish_and_clear();

            let elapsed = start_time.elapsed();
            let speed = if elapsed.as_secs() > 0 {
                downloaded as f64 / elapsed.as_secs_f64()
            } else {
                0.0
            };

            println!(
                "  ✓ {} ({:.2} MB) - {:.2} MB/s",
                style("Done").green(),
                downloaded as f64 / 1024.0 / 1024.0,
                speed / 1024.0 / 1024.0
            );
        }

        std::fs::rename(&tmp_path, &final_path)
            .with_context(|| format!("Failed to finalize {}", task.filename))?;

        main_pb.inc(1);
    }

    main_pb.finish_and_clear();
    println!("\n{} All downloads completed!", style("✓").green().bold());

    Ok(())
}

/// Main download orchestrator.
pub async fn download_courses(
    tasks: &[DownloadTask],
    output_dir: &Path,
    no_record: bool,
    client: &reqwest::Client,
) -> Result<()> {
    if tasks.is_empty() {
        println!("No videos to download.");
        return Ok(());
    }

    println!("{} videos to download.", tasks.len());
    println!("{}", style("Downloader preference:").cyan());
    println!("  - aria2c (default)");
    println!("  - built-in downloader (fallback)\n");

    // Record in history
    if !no_record {
        let links: Vec<String> = tasks.iter().map(|t| t.url.clone()).collect();
        let filenames: Vec<String> = tasks.iter().map(|t| t.filename.clone()).collect();
        let dirname = output_dir.to_string_lossy().to_string();

        history::add_history(HistoryEntry {
            time: chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
            links,
            filenames,
            video_dirname: dirname,
        });
    }

    // Default: aria2c. Fallback: reqwest streaming.
    match run_default_aria2c_download(tasks, output_dir) {
        Ok(()) => {
            println!("\n{} aria2c download completed!", style("✓").green().bold());
            return Ok(());
        }
        Err(Aria2cError::NotFound) => {
            println!(
                "{} aria2c not found, using built-in downloader...",
                style("!").yellow().bold()
            );
        }
        Err(Aria2cError::LaunchFailed(e)) => {
            println!(
                "{} aria2c failed to start: {}\n{} Using built-in downloader...",
                style("!").yellow().bold(),
                e,
                style("Info:").yellow()
            );
        }
        Err(Aria2cError::NonZeroExit(status)) => {
            println!(
                "{} aria2c exited with status {}, falling back to built-in downloader...",
                style("!").yellow().bold(),
                status
            );
        }
        Err(Aria2cError::PrepareInputFailed(e)) => {
            // local prepare errors should be surfaced; don't pretend aria2c is unavailable
            return Err(e);
        }
    }

    download_with_reqwest(client, tasks, output_dir).await
}

/// Re-download from history entry.
pub async fn redownload_from_history(index: usize, client: &reqwest::Client) -> Result<()> {
    let entries = history::get_history();
    if index >= entries.len() {
        return Err(anyhow::anyhow!(
            "History index {} out of range (total: {})",
            index,
            entries.len()
        ));
    }

    let entry = &entries[index];
    let output_dir = PathBuf::from(&entry.video_dirname);
    let tasks: Vec<DownloadTask> = entry
        .links
        .iter()
        .zip(entry.filenames.iter())
        .map(|(url, filename)| DownloadTask {
            url: url.clone(),
            filename: filename.clone(),
        })
        .collect();

    println!(
        "Re-downloading {} files to {}...",
        tasks.len(),
        output_dir.display()
    );
    download_courses(&tasks, &output_dir, true, client).await
}
