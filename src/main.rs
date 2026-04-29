use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;

mod api;
mod app;
mod client;
mod config;
mod download;
mod history;
mod logging;
mod login;
mod types;

use crate::logging::set_verbose;

#[derive(Debug, Clone, Deserialize)]
struct LoginFile {
    username: String,
    password: String,
}

/// Login credentials that can be passed to any command requiring authentication.
#[derive(Parser, Clone)]
struct LoginOpts {
    /// Read credentials from a TOML file (default: ./login.toml).
    ///
    /// The file should contain:
    /// username = "..."
    /// password = "..."
    #[arg(long, short = 'f', default_value = "login.toml")]
    file: String,

    /// jAccount username
    #[arg(long, short = 'u')]
    username: Option<String>,

    /// jAccount password
    #[arg(long, short = 'p')]
    password: Option<String>,
}

#[derive(Parser)]
#[command(
    name = "sjtu-canvas-video-download",
    about = "SJTU Canvas Video Downloader (Rust CLI)"
)]
struct Cli {
    /// Print verbose diagnostic traces (debug output)
    #[arg(short, long, global = true, action = clap::ArgAction::SetTrue)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive jAccount login (username/password)
    Login {
        #[command(flatten)]
        login: LoginOpts,
    },
    /// Download videos
    Download {
        /// Use a specific Canvas course ID (v2 API)
        #[arg(long)]
        course_id: String,

        /// Lecture selector: 0 (all), N (1-based), or A-B (1-based)
        #[arg(long)]
        lecture: Option<String>,

        /// Only download main recordings (skip screencasts)
        #[arg(long)]
        only_recordings: bool,
        /// Output directory (default: ./videos)
        #[arg(long, default_value = "./videos")]
        output_dir: String,
    },
    /// View and manage download history
    History {
        /// Re-download a history entry by index
        #[arg(long)]
        re_download: Option<usize>,
        /// Clear all history
        #[arg(long)]
        clear: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    set_verbose(cli.verbose);

    match cli.command {
        Commands::Login { login } => cmd_login(login).await?,
        Commands::Download {
            course_id,
            lecture,
            only_recordings,
            output_dir,
        } => {
            cmd_download(course_id, lecture, only_recordings, output_dir).await?;
        }
        Commands::History { re_download, clear } => cmd_history(re_download, clear).await?,
    }

    Ok(())
}

async fn cmd_login(login: LoginOpts) -> Result<()> {
    let app = app::App::new().await?;

    let (username, password) = get_login_credentials(&login)?;

    let username = username.unwrap_or_else(|| {
        dialoguer::Input::new()
            .with_prompt("Username")
            .interact_text()
            .unwrap_or_default()
    });

    let password = password.unwrap_or_else(|| {
        dialoguer::Password::new()
            .with_prompt("Password")
            .interact()
            .unwrap_or_default()
    });

    app.login_pwd(&username, &password, None).await?;
    Ok(())
}

async fn cmd_download(
    course_id: String,
    lecture: Option<String>,
    only_recordings: bool,
    output_dir: String,
) -> Result<()> {
    let mut app = app::App::new().await?;

    // Fetch course by required course-id
    app.set_course_id(course_id);
    app.refresh_courses_v2_with_auth_retry().await?;

    let output_path = PathBuf::from(&output_dir);

    match parse_lecture_spec(lecture.as_deref())? {
        LectureSpec::All => {
            app.download_all_lectures(only_recordings, &output_path)
                .await?;
        }
        LectureSpec::One(n) => {
            app.download_lecture_range(n, n, only_recordings, &output_path)
                .await?;
        }
        LectureSpec::Range(start, end) => {
            app.download_lecture_range(start, end, only_recordings, &output_path)
                .await?;
        }
    }

    Ok(())
}

enum LectureSpec {
    All,
    One(usize),
    Range(usize, usize),
}

fn load_login_file(path: &str) -> Result<LoginFile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read login file: {}", path))?;
    toml::from_str(&content)
        .with_context(|| format!("Failed to parse login file as TOML: {}", path))
}

fn get_login_credentials(login: &LoginOpts) -> Result<(Option<String>, Option<String>)> {
    let has_user = login.username.is_some();
    let has_pass = login.password.is_some();

    if has_user || has_pass {
        if !(has_user && has_pass) {
            return Err(anyhow::anyhow!(
                "--username/-u and --password/-p must be provided together"
            ));
        }
        return Ok((login.username.clone(), login.password.clone()));
    }

    // Fallback to file
    let lf = load_login_file(&login.file)?;
    Ok((Some(lf.username), Some(lf.password)))
}

fn parse_lecture_spec(s: Option<&str>) -> Result<LectureSpec> {
    let Some(s) = s else {
        return Ok(LectureSpec::One(1));
    };

    let s = s.trim();
    if s.is_empty() {
        return Ok(LectureSpec::One(1));
    }

    if s == "0" {
        return Ok(LectureSpec::All);
    }

    if let Ok(n) = s.parse::<usize>() {
        if n == 0 {
            return Err(anyhow::anyhow!(
                "Lecture number must be >= 1 (use 0 for all)"
            ));
        }
        return Ok(LectureSpec::One(n));
    }

    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!(
            "Invalid lecture selector: {}. Use 0 (all), N, or A-B (e.g. 1-5)",
            s
        ));
    }

    let start: usize = parts[0].trim().parse()?;
    let end: usize = parts[1].trim().parse()?;

    if start == 0 || end == 0 {
        return Err(anyhow::anyhow!("Lecture range endpoints must be >= 1"));
    }
    if start > end {
        return Err(anyhow::anyhow!(
            "Invalid lecture range: start ({}) > end ({})",
            start,
            end
        ));
    }

    Ok(LectureSpec::Range(start, end))
}

async fn cmd_history(re_download: Option<usize>, clear: bool) -> Result<()> {
    if clear {
        println!("Clearing history...");
        history::clear_history()?;
        println!("History cleared.");
        return Ok(());
    }

    if let Some(idx) = re_download {
        let app = app::App::new().await?;
        download::redownload_from_history(idx, &app.client).await?;
        return Ok(());
    }

    // Print history
    let entries = history::get_history();
    if entries.is_empty() {
        println!("No download history.");
        return Ok(());
    }

    println!("Download history:");
    for (i, entry) in entries.iter().enumerate() {
        let ts = chrono::DateTime::from_timestamp_nanos(entry.time);
        let first_file = entry.filenames.first().map(|s| s.as_str()).unwrap_or("N/A");
        println!(
            "  [{}] {} - {} ({} files)",
            i,
            ts.format("%Y-%m-%d %H:%M:%S"),
            first_file,
            entry.filenames.len()
        );
    }

    println!("\nUse --re-download <index> to re-download, or --clear to clear history.");
    Ok(())
}
