use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use console::style;

use crate::types::{Course, DownloadTask, HistoryEntry};
use crate::history;

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
    // Use std::env::PATH to search
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in path_env.split(':') {
            let exe_path = Path::new(dir).join(if cfg!(windows) { "aria2c.exe" } else { "aria2c" });
            if exe_path.exists() {
                return Some(exe_path.to_string_lossy().to_string());
            }
        }
    }
    None
}

/// Sanitize a filename for the filesystem.
pub fn sanitize_filename(name: &str) -> String {
    name.replace(|c: char| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'), "_")
}

/// Generate download tasks from selected courses.
pub fn generate_download_tasks(
    courses: &[Course],
    only_recordings: bool,
) -> Vec<DownloadTask> {
    let mut tasks = Vec::new();
    let mut global_index = 1;

    for course in courses {
        let mut sorted_videos = course.videos.clone();
        sorted_videos.sort_by_key(|v| v.view_num);

        for video in &sorted_videos {
            if only_recordings && !video.is_recording {
                continue;
            }

            let ext = if video.file_ext.is_empty() { "mp4" } else { &video.file_ext };
            let filename = format!(
                "{}_{}_{}_{:03}.{}",
                sanitize_filename(&course.subject_name),
                sanitize_filename(&course.teacher),
                sanitize_filename(&course.name),
                global_index,
                ext,
            );

            tasks.push(DownloadTask {
                url: video.url.clone(),
                filename,
            });

            global_index += 1;
        }
    }

    tasks
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

/// Download using aria2c with an input file.
pub fn download_with_aria2c(tasks: &[DownloadTask], output_dir: &Path) -> Result<()> {
    let aria2c = find_aria2c()
        .ok_or_else(|| anyhow::anyhow!("aria2c not found"))?;

    // Generate aria2c input file
    let tmp_dir = output_dir.join(".sjtu_canvas_tmp");
    std::fs::create_dir_all(&tmp_dir)?;

    let input_path = tmp_dir.join(format!("aria2_{}.txt", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));

    let file = File::create(&input_path)
        .context("Failed to write aria2c input file")?;
    let mut writer = BufWriter::new(file);

    for task in tasks {
        writeln!(writer, "{}", task.url)?;
        writeln!(writer, "  out={}", task.filename)?;
        writeln!(writer, "  header=referer: https://v.sjtu.edu.cn")?;
    }
    writer.flush()?;

    println!("\n{}", style("Starting aria2c download...").green().bold());
    println!("{} {}", style("Output directory:").cyan(), output_dir.display());
    println!("{} {}", style("Total files:").cyan(), tasks.len());
    println!("{} This may take a while...\n", style("Info:").yellow());

    let mut cmd = Command::new(&aria2c);
    cmd.arg("-d").arg(output_dir)
        .arg("-i").arg(&input_path)
        .arg("-x").arg("16")
        .arg("--auto-file-renaming=false")
        .arg("--summary-interval=1"); // Update progress every second

    let status = cmd.status().context("Failed to start aria2c")?;

    let _ = std::fs::remove_file(&input_path);
    let _ = std::fs::remove_dir(&tmp_dir);

    if status.success() {
        println!("\n{} aria2c download completed!", style("✓").green().bold());
        Ok(())
    } else {
        Err(anyhow::anyhow!("aria2c exited with non-zero status: {:?}", status))
    }
}

/// Fallback: download each file using reqwest streaming with progress bar.
pub async fn download_with_reqwest(
    client: &reqwest::Client,
    tasks: &[DownloadTask],
    output_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(output_dir)
        .context("Failed to create output directory")?;

    let total = tasks.len();

    // Overall progress bar for all files
    let main_pb = ProgressBar::new(total as u64);
    main_pb.set_style(
        ProgressStyle::with_template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} files ({eta})")
            .unwrap()
            .progress_chars("█▓▒░  ")
    );

    for (i, task) in tasks.iter().enumerate() {
        main_pb.set_message(format!("Downloading {}", task.filename));
        main_pb.tick();

        println!("\n[{}/{}] {}", i + 1, total, style(&task.filename).cyan().bold());

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

    // Try aria2c first, fall back to reqwest streaming
    match download_with_aria2c(tasks, output_dir) {
        Ok(_) => return Ok(()),
        Err(e) => {
            println!("aria2c not available: {}", e);
            println!("Falling back to built-in downloader (slower)...");
        }
    }

    download_with_reqwest(client, tasks, output_dir).await
}

/// Re-download from history entry.
pub async fn redownload_from_history(
    index: usize,
    client: &reqwest::Client,
) -> Result<()> {
    let entries = history::get_history();
    if index >= entries.len() {
        return Err(anyhow::anyhow!("History index {} out of range (total: {})", index, entries.len()));
    }

    let entry = &entries[index];
    let output_dir = PathBuf::from(&entry.video_dirname);
    let tasks: Vec<DownloadTask> = entry.links.iter()
        .zip(entry.filenames.iter())
        .map(|(url, filename)| DownloadTask {
            url: url.clone(),
            filename: filename.clone(),
        })
        .collect();

    println!("Re-downloading {} files to {}...", tasks.len(), output_dir.display());
    download_courses(&tasks, &output_dir, true, client).await
}
