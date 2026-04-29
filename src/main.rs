use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod app;
mod api;
mod client;
mod config;
mod download;
mod history;
mod login;
mod qr_login;
mod types;

/// Login credentials that can be passed to any command requiring authentication.
#[derive(Parser, Clone)]
struct LoginOpts {
    /// jAccount username
    #[arg(long)]
    username: Option<String>,

    /// jAccount password
    #[arg(long)]
    password: Option<String>,

    /// Captcha text (image will be saved to /tmp/sjtu_captcha.png)
    #[arg(long)]
    captcha: Option<String>,
}

#[derive(Parser)]
#[command(name = "sjtu-canvas-video-download", about = "SJTU Canvas Video Downloader (Rust CLI)")]
struct Cli {
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
    /// Interactive jAccount QR code login
    QrLogin,
    /// List all enrolled courses and videos
    List {
        /// Use a specific Canvas course ID (v2 API)
        #[arg(long)]
        course_id: Option<String>,

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
    /// Export course URLs to JSON
    Export {
        /// Output file path
        path: String,
        /// Use a specific Canvas course ID (v2 API)
        #[arg(long)]
        course_id: Option<String>,

        #[command(flatten)]
        login: LoginOpts,
    },
    /// Import course URLs from JSON and download
    Import {
        /// Input file path
        path: String,
        /// Only download main recordings
        #[arg(long)]
        only_recordings: bool,
        /// Output directory
        #[arg(long, default_value = "./videos")]
        output_dir: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Login { login } => cmd_login(login).await?,
        Commands::QrLogin => cmd_qr_login().await?,
        Commands::List { course_id, login } => cmd_list(course_id, login).await?,
        Commands::Download { course_id, lecture, only_recordings, output_dir } => {
            cmd_download(course_id, lecture, only_recordings, output_dir).await?;
        }
        Commands::History { re_download, clear } => cmd_history(re_download, clear).await?,
        Commands::Export { path, course_id, login } => cmd_export(path, course_id, login).await?,
        Commands::Import { path, only_recordings, output_dir } => {
            cmd_import(path, only_recordings, output_dir).await?;
        }
    }

    Ok(())
}

/// Try to login if credentials are provided, otherwise rely on saved cookies.
async fn maybe_login(app: &app::App, login: &LoginOpts) -> Result<()> {
    if login.username.is_some() || login.password.is_some() {
        let username = login.username.clone().unwrap_or_default();
        let password = login.password.clone().unwrap_or_default();
        app.login_pwd(&username, &password, login.captcha.clone()).await?;
    }
    Ok(())
}

async fn cmd_login(login: LoginOpts) -> Result<()> {
    let app = app::App::new().await?;

    let username = login.username.unwrap_or_else(|| {
        dialoguer::Input::new()
            .with_prompt("Username")
            .interact_text()
            .unwrap_or_default()
    });

    let password = login.password.unwrap_or_else(|| {
        dialoguer::Password::new()
            .with_prompt("Password")
            .interact()
            .unwrap_or_default()
    });

    app.login_pwd(&username, &password, login.captcha).await?;
    Ok(())
}

async fn cmd_qr_login() -> Result<()> {
    let app = app::App::new().await?;
    app.login_qr().await?;
    Ok(())
}

async fn cmd_list(course_id: Option<String>, login: LoginOpts) -> Result<()> {
    let mut app = app::App::new().await?;
    maybe_login(&app, &login).await?;

    if let Some(id) = course_id {
        app.set_course_id(id);
        app.refresh_courses_v2().await?;
    } else {
        app.refresh_courses_default().await?;
    }

    app.print_courses();
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
    app.refresh_courses_v2().await?;

    let output_path = PathBuf::from(&output_dir);

    match parse_lecture_spec(lecture.as_deref())? {
        LectureSpec::All => {
            app.download_all_lectures(only_recordings, &output_path).await?;
        }
        LectureSpec::One(n) => {
            app.download_lecture_range(n, n, only_recordings, &output_path).await?;
        }
        LectureSpec::Range(start, end) => {
            app.download_lecture_range(start, end, only_recordings, &output_path).await?;
        }
    }

    Ok(())
}

enum LectureSpec {
    All,
    One(usize),
    Range(usize, usize),
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
            return Err(anyhow::anyhow!("Lecture number must be >= 1 (use 0 for all)"));
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
        println!("  [{}] {} - {} ({} files)", i, ts.format("%Y-%m-%d %H:%M:%S"), first_file, entry.filenames.len());
    }

    println!("\nUse --re-download <index> to re-download, or --clear to clear history.");
    Ok(())
}

async fn cmd_export(path: String, course_id: Option<String>, login: LoginOpts) -> Result<()> {
    let mut app = app::App::new().await?;
    maybe_login(&app, &login).await?;

    if let Some(id) = course_id {
        app.set_course_id(id);
        app.refresh_courses_v2().await?;
    } else {
        app.refresh_courses_default().await?;
    }

    app.export_courses(&PathBuf::from(&path))?;
    Ok(())
}

async fn cmd_import(path: String, only_recordings: bool, output_dir: String) -> Result<()> {
    let app = app::App::new().await?;
    let courses = app.import_courses(&PathBuf::from(&path))?;

    let tasks = download::generate_download_tasks(&courses, only_recordings);
    if tasks.is_empty() {
        return Err(anyhow::anyhow!("No videos found in imported data"));
    }

    println!("\nDownload preview:");
    println!("{}", download::preview_tasks(&tasks));
    println!();

    download::download_courses(&tasks, &PathBuf::from(&output_dir), false, &app.client).await?;
    Ok(())
}
