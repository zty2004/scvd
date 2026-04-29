use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config;
use crate::types::{Course, DownloadTask, VideoInfo};
use crate::login;
use crate::qr_login;
use crate::api;
use crate::download;

/// Application state.
pub struct App {
    pub client: reqwest::Client,
    pub course_id: Option<String>,
    pub courses: Vec<Course>,
}

impl App {
    pub async fn new() -> Result<Self> {
        let client = crate::client::create_client()?;
        let saved_config = config::load_config()?;

        Ok(Self {
            client,
            course_id: saved_config.course_id.clone(),
            courses: Vec::new(),
        })
    }

    /// Login with username/password + captcha.
    pub async fn login_pwd(&self, username: &str, password: &str, captcha_opt: Option<String>) -> Result<()> {
        println!("Connecting to jAccount login...");
        let login_info = login::get_params_uuid_cookies(&self.client, login::CANVAS_LOGIN_URL).await?;
        println!("Login page loaded. Fetching captcha...");

        let captcha_bytes = login::get_captcha_bytes(&self.client, &login_info.uuid, &login_info.final_url).await?;

        let captcha = if let Some(c) = captcha_opt {
            // Save captcha image to file so user can view it
            let captcha_path = std::env::temp_dir().join("sjtu_captcha.png");
            std::fs::write(&captcha_path, &captcha_bytes)?;
            println!("Captcha image saved to: {}", captcha_path.display());
            c
        } else {
            // Try terminal display, fallback to file
            if let Ok(img) = image::load_from_memory(&captcha_bytes) {
                if viuer::print(&img, &viuer::Config::default()).is_ok() {
                    dialoguer::Input::new()
                        .with_prompt("Captcha text")
                        .interact_text()
                        .unwrap_or_default()
                } else {
                    let captcha_path = std::env::temp_dir().join("sjtu_captcha.png");
                    std::fs::write(&captcha_path, &captcha_bytes)?;
                    println!("Captcha image saved to: {}", captcha_path.display());
                    dialoguer::Input::new()
                        .with_prompt("Captcha text")
                        .interact_text()
                        .unwrap_or_default()
                }
            } else {
                let captcha_path = std::env::temp_dir().join("sjtu_captcha.png");
                std::fs::write(&captcha_path, &captcha_bytes)?;
                println!("Captcha image saved to: {}", captcha_path.display());
                dialoguer::Input::new()
                    .with_prompt("Captcha text")
                    .interact_text()
                    .unwrap_or_default()
            }
        };

        println!("Submitting login...");
        let success = login::login(
            &self.client, username, password,
            &login_info.uuid, &captcha, &login_info.params,
        ).await?;

        if !success {
            return Err(anyhow::anyhow!("Login failed. Wrong captcha or credentials?"));
        }

        println!("Login successful!");

        // Establish OC session
        println!("Establishing OpenCourse session...");
        login::login_using_cookies(&self.client, login::OC_LOGIN_URL).await?;

        // Save cookies for future commands
        println!("Session cookies saved.");

        Ok(())
    }

    /// Login with QR code. Runs WebSocket monitor in a background thread.
    pub async fn login_qr(&self) -> Result<()> {
        println!("Connecting to jAccount QR login...");
        let login_info = login::get_params_uuid_cookies(&self.client, login::CANVAS_LOGIN_URL).await?;

        let client = self.client.clone();
        let uuid = login_info.uuid.clone();

        // WebSocket monitor in background thread
        let ws_uuid = uuid.clone();
        let ws_handle = std::thread::spawn(move || -> Result<()> {
            let client_clone = client.clone();
            let ws_uuid_inner = ws_uuid.clone();

            qr_login::monitor_qr_ws(&ws_uuid, "", move |ts, sig| {
                // Fetch and display new QR code
                let rt = tokio::runtime::Runtime::new().unwrap();
                let qr_bytes = rt.block_on(async {
                    qr_login::get_qr_code_bytes(&client_clone, &ws_uuid_inner, &ts, &sig).await
                });
                if let Ok(bytes) = qr_bytes {
                    qr_login::display_qr(&bytes);
                }
                Ok(())
            })
        });

        // Initial QR code
        let qr_bytes = qr_login::get_qr_code_bytes(&self.client, &uuid, "0", "0").await?;
        qr_login::display_qr(&qr_bytes);
        println!("\nScan the QR code with your SJTU mobile app...");

        // Wait for WebSocket thread to complete (returns on LOGIN event)
        match ws_handle.join() {
            Ok(Ok(())) => {
                println!("QR code scanned! Completing login...");
                let success = qr_login::express_login(&self.client, &uuid).await?;
                if !success {
                    return Err(anyhow::anyhow!("QR login completion failed"));
                }

                println!("Login successful!");

                // Establish OC session
                println!("Establishing OpenCourse session...");
                login::login_using_cookies(&self.client, login::OC_LOGIN_URL).await?;

                // Save cookies for future commands
                println!("Session cookies saved.");
            }
            Ok(Err(e)) => {
                return Err(anyhow::anyhow!("QR login WebSocket error: {}", e));
            }
            Err(_) => {
                return Err(anyhow::anyhow!("QR login thread panicked"));
            }
        }

        Ok(())
    }

    /// Fetch all courses using the default VOD API.
    pub async fn refresh_courses_default(&mut self) -> Result<()> {
        println!("Fetching enrolled courses...");
        self.courses = api::get_all_courses(&self.client).await?;
        println!("Found {} courses.", self.courses.len());
        Ok(())
    }

    /// Fetch courses using the v2 OIDC/LTI3 flow (course ID mode).
    pub async fn refresh_courses_v2(&mut self) -> Result<()> {
        let course_id = self.course_id.as_ref()
            .ok_or_else(|| anyhow::anyhow!("No course ID set. Use --course-id or set it via config."))?
            .clone();

        println!("Fetching course {} via v2 API...", course_id);
        self.courses = api::get_real_canvas_videos_v2(&self.client, &course_id).await?;
        println!("Found {} courses.", self.courses.len());
        Ok(())
    }

    /// Print the course list.
    pub fn print_courses(&self) {
        if self.courses.is_empty() {
            println!("No courses found.");
            return;
        }

        for (i, course) in self.courses.iter().enumerate() {
            println!("\n[{}] {}", i + 1, course.name);
            if !course.teacher.is_empty() {
                println!("    Teacher: {}", course.teacher);
            }
            for (j, video) in course.videos.iter().enumerate() {
                let kind = if video.is_recording { "REC" } else { "SCR" };
                println!("  {:>3}. [{:3}] {}", j + 1, kind, video.name);
                if !video.url.is_empty() {
                    println!("         URL: {}", video.url);
                }
            }
        }
    }

    /// Download selected courses.
    pub async fn _download(
        &self,
        course_indices: &[usize],
        only_recordings: bool,
        output_dir: &PathBuf,
    ) -> Result<()> {
        let selected: Vec<Course> = course_indices
            .iter()
            .filter_map(|&i| self.courses.get(i).cloned())
            .collect();

        if selected.is_empty() {
            return Err(anyhow::anyhow!("No courses selected"));
        }

        let tasks = download::generate_download_tasks(&selected, only_recordings);
        if tasks.is_empty() {
            return Err(anyhow::anyhow!("No videos found in selected courses"));
        }

        println!("\nDownload preview:");
        println!("{}", download::preview_tasks(&tasks));
        println!();

        download::download_courses(&tasks, output_dir, false, &self.client).await
    }

    /// Download a specific range of videos from a course.
    pub async fn _download_range(
        &self,
        course_index: usize,
        start: usize,
        end: usize,
        only_recordings: bool,
        output_dir: &PathBuf,
    ) -> Result<()> {
        let course = self.courses.get(course_index)
            .ok_or_else(|| anyhow::anyhow!("Course index {} out of range", course_index + 1))?;

        let mut sorted_videos = course.videos.clone();
        sorted_videos.sort_by_key(|v| v.view_num);

        let start_idx = start.saturating_sub(1); // 1-indexed
        let end_idx = end.min(sorted_videos.len());

        if start_idx >= end_idx {
            return Err(anyhow::anyhow!("Invalid range: {}-{} (total: {} videos)", start, end, sorted_videos.len()));
        }

        let mut tasks: Vec<DownloadTask> = Vec::new();
        let mut global_idx = 1;

        for (local_idx, video) in sorted_videos.iter().enumerate() {
            if local_idx < start_idx || local_idx >= end_idx {
                continue;
            }
            if only_recordings && !video.is_recording {
                continue;
            }

            let ext = if video.file_ext.is_empty() { "mp4" } else { &video.file_ext };
            tasks.push(DownloadTask {
                url: video.url.clone(),
                filename: format!(
                    "{}_{}_{}_{:03}.{}",
                    download::sanitize_filename(&course.subject_name),
                    download::sanitize_filename(&course.teacher),
                    download::sanitize_filename(&course.name),
                    global_idx,
                    ext,
                ),
            });
            global_idx += 1;
        }

        if tasks.is_empty() {
            return Err(anyhow::anyhow!("No videos in the selected range"));
        }

        println!("\nDownload preview:");
        println!("{}", download::preview_tasks(&tasks));
        println!();

        download::download_courses(&tasks, output_dir, false, &self.client).await
    }

    /// Download lectures (v2 API) by 1-based lecture index range.
    ///
    /// Note: the v2 fetch may return multiple Course entries for one course-id.
    /// We flatten all videos across those entries, then sort by view_num.
    pub async fn download_lecture_range(
        &self,
        start: usize,
        end: usize,
        only_recordings: bool,
        output_dir: &PathBuf,
    ) -> Result<()> {
        if start == 0 || end == 0 {
            return Err(anyhow::anyhow!("Lecture numbers must be >= 1"));
        }
        if start > end {
            return Err(anyhow::anyhow!("Invalid lecture range: start ({}) > end ({})", start, end));
        }

        if self.courses.is_empty() {
            return Err(anyhow::anyhow!("No course data loaded"));
        }

        let (subject_name, teacher, course_name) = {
            let first = &self.courses[0];
            (first.subject_name.clone(), first.teacher.clone(), first.name.clone())
        };

        let mut all_videos: Vec<VideoInfo> = self
            .courses
            .iter()
            .flat_map(|c| c.videos.clone())
            .collect();

        if all_videos.is_empty() {
            return Err(anyhow::anyhow!("No videos found for this course"));
        }

        all_videos.sort_by_key(|v| v.view_num);

        let total = all_videos.len();
        if start > total || end > total {
            return Err(anyhow::anyhow!(
                "Lecture range {}-{} out of range (total: {} lectures)",
                start,
                end,
                total
            ));
        }

        let start_idx = start - 1; // 1-based
        let end_idx_excl = end; // 1-based, inclusive -> exclusive

        let mut tasks: Vec<DownloadTask> = Vec::new();
        let mut global_idx = start; // keep numbering aligned with lecture index

        for (local_idx, video) in all_videos.iter().enumerate() {
            if local_idx < start_idx || local_idx >= end_idx_excl {
                continue;
            }
            if only_recordings && !video.is_recording {
                continue;
            }

            let ext = if video.file_ext.is_empty() { "mp4" } else { &video.file_ext };
            tasks.push(DownloadTask {
                url: video.url.clone(),
                filename: format!(
                    "{}_{}_{}_{:03}.{}",
                    download::sanitize_filename(&subject_name),
                    download::sanitize_filename(&teacher),
                    download::sanitize_filename(&course_name),
                    global_idx,
                    ext,
                ),
            });
            global_idx += 1;
        }

        if tasks.is_empty() {
            if only_recordings {
                return Err(anyhow::anyhow!(
                    "No videos left after applying --only-recordings to the selected lectures"
                ));
            }
            return Err(anyhow::anyhow!("No videos in the selected lecture range"));
        }

        println!("\nDownload preview:");
        println!("{}", download::preview_tasks(&tasks));
        println!();

        download::download_courses(&tasks, output_dir, false, &self.client).await
    }

    /// Download all lectures (v2 API).
    pub async fn download_all_lectures(
        &self,
        only_recordings: bool,
        output_dir: &PathBuf,
    ) -> Result<()> {
        let total = self.courses.iter().map(|c| c.videos.len()).sum::<usize>();
        if total == 0 {
            return Err(anyhow::anyhow!("No videos found for this course"));
        }
        self.download_lecture_range(1, total, only_recordings, output_dir).await
    }

    /// Export course data to JSON.
    pub fn export_courses(&self, path: &PathBuf) -> Result<()> {
        let content = serde_json::to_string_pretty(&self.courses)
            .context("Failed to serialize courses")?;
        std::fs::write(path, content)
            .context("Failed to write export file")?;
        println!("Exported {} courses to {}", self.courses.len(), path.display());
        Ok(())
    }

    /// Import course data from JSON.
    pub fn import_courses(&self, path: &PathBuf) -> Result<Vec<Course>> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let courses: Vec<Course> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        println!("Imported {} courses from {}", courses.len(), path.display());
        Ok(courses)
    }

    /// Set course ID.
    pub fn set_course_id(&mut self, id: String) {
        self.course_id = Some(id);
    }
}
