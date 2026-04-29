use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::api;
use crate::config;
use crate::download;
use crate::login;
use crate::types::{Course, DownloadTask, VideoInfo};
use crate::vdebug;

fn print_captcha_in_terminal(img: &image::DynamicImage) -> Result<()> {
    // Add some vertical spacing so we don't overwrite recent terminal output.
    println!("\n\n--- CAPTCHA ---\n");

    // Use a fixed placement to avoid cursor repositioning artifacts.
    // Using absolute positioning can force output to the top-left depending on terminal.
    let mut cfg = viuer::Config::default();
    cfg.absolute_offset = false;
    cfg.x = 0;
    cfg.y = 0;

    viuer::print(img, &cfg).context("Failed to print captcha image to terminal")?;

    println!("\n---------------\n");
    Ok(())
}

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

    async fn prewarm_oc_cookies(&self) {
        // Best-effort: try to let oc.sjtu.edu.cn set session/csrf cookies, then persist
        // any Set-Cookie headers to cookies.json.
        let url = "https://oc.sjtu.edu.cn/";
        match self.client.get(url).send().await {
            Ok(resp) => {
                if let Err(e) = crate::client::capture_and_save_cookies(&resp, "oc.sjtu.edu.cn") {
                    vdebug!("[DEBUG] prewarm oc cookies: failed to save cookies: {}", e);
                } else {
                    vdebug!("[DEBUG] prewarm oc cookies: done");
                }
            }
            Err(e) => {
                vdebug!("[DEBUG] prewarm oc cookies: request failed: {}", e);
            }
        }
    }

    fn load_saved_credentials() -> Result<(String, String)> {
        let cfg = config::load_config()?;
        match (cfg.username, cfg.password) {
            (Some(u), Some(p)) => Ok((u, p)),
            _ => Err(anyhow::anyhow!(
                "Session expired, but no saved credentials found in config.json. \
Please run the login command first (and save username/password) so download can auto re-login."
            )),
        }
    }

    async fn relogin_from_saved_credentials(&mut self) -> Result<()> {
        let (username, password) = Self::load_saved_credentials()?;

        // Reuse existing interactive login flow (captcha may be required).
        self.login_pwd(&username, &password, None).await?;

        // Re-create client so subsequent requests use refreshed cookie state.
        self.client = crate::client::create_client()?;
        Ok(())
    }

    /// Login with username/password + captcha.
    pub async fn login_pwd(
        &self,
        username: &str,
        password: &str,
        captcha_opt: Option<String>,
    ) -> Result<()> {
        println!("Connecting to jAccount login...");
        let login_info =
            login::get_params_uuid_cookies(&self.client, login::CANVAS_LOGIN_URL).await?;
        println!("Login page loaded. Fetching captcha...");

        let captcha_bytes =
            login::get_captcha_bytes(&self.client, &login_info.uuid, &login_info.final_url).await?;

        let captcha = if let Some(c) = captcha_opt {
            // Save captcha image to file so user can view it
            let captcha_path = std::env::temp_dir().join("sjtu_captcha.png");
            std::fs::write(&captcha_path, &captcha_bytes)?;
            println!("Captcha image saved to: {}", captcha_path.display());
            c
        } else {
            // Try terminal display, fallback to file
            if let Ok(img) = image::load_from_memory(&captcha_bytes) {
                if print_captcha_in_terminal(&img).is_ok() {
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
            &self.client,
            username,
            password,
            &login_info.uuid,
            &captcha,
            &login_info.params,
        )
        .await?;

        if !success {
            return Err(anyhow::anyhow!(
                "Login failed. Wrong captcha or credentials (or captcha expired)."
            ));
        }

        println!("Login successful!");

        // NOTE: Stop here. Do NOT perform any follow-up OpenCourse/Canvas queries.
        // The login command should only validate credentials + captcha and then exit.
        println!("Session cookies saved.");

        Ok(())
    }

    /// Fetch all courses using the default VOD API.
    #[allow(dead_code)]
    pub async fn refresh_courses_default(&mut self) -> Result<()> {
        println!("Fetching enrolled courses...");
        self.courses = api::get_all_courses(&self.client).await?;
        println!("Found {} courses.", self.courses.len());
        Ok(())
    }

    /// Fetch courses using the v2 OIDC/LTI3 flow (course ID mode).
    pub async fn refresh_courses_v2(&mut self) -> Result<()> {
        let course_id = self
            .course_id
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!("No course ID set. Use --course-id or set it via config.")
            })?
            .clone();

        println!("Fetching course {} via v2 API...", course_id);
        self.courses = api::get_real_canvas_videos_v2(&self.client, &course_id).await?;
        println!("Found {} courses.", self.courses.len());
        Ok(())
    }

    /// Fetch courses using the v2 API, with a single automatic re-login retry
    /// when the failure looks like an expired auth session.
    pub async fn refresh_courses_v2_with_auth_retry(&mut self) -> Result<()> {
        let course_id = self
            .course_id
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!("No course ID set. Use --course-id or set it via config.")
            })?
            .clone();

        // Best-effort cookie prewarm for oc.sjtu.edu.cn, to improve the chance of
        // finding the OIDC/LTI forms in the following v2 flow.
        self.prewarm_oc_cookies().await;

        println!("Fetching course {} via v2 API...", course_id);

        match api::get_real_canvas_videos_v2(&self.client, &course_id).await {
            Ok(courses) => {
                self.courses = courses;
                println!("Found {} courses.", self.courses.len());
                Ok(())
            }
            Err(e) => {
                if !api::is_retryable_v2_auth_error(&e) {
                    return Err(e);
                }

                println!("Session expired. Attempting automatic re-login...");
                self.relogin_from_saved_credentials().await?;

                println!("Re-login successful. Retrying v2 fetch...");
                self.courses = api::get_real_canvas_videos_v2(&self.client, &course_id).await?;
                println!("Found {} courses.", self.courses.len());
                Ok(())
            }
        }
    }

    /// Print the course list.
    #[allow(dead_code)]
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

        let mut tasks: Vec<DownloadTask> = Vec::new();
        let mut global_index = 1;

        for course in &selected {
            let mut sorted_videos = course.videos.clone();
            sorted_videos.sort_by_key(|v| v.view_num);

            for video in &sorted_videos {
                if only_recordings && !video.is_recording {
                    continue;
                }

                let ext = if video.file_ext.is_empty() {
                    "mp4"
                } else {
                    &video.file_ext
                };
                let filename = format!(
                    "{}_{}_{}_{:03}.{}",
                    download::sanitize_filename(&course.subject_name),
                    download::sanitize_filename(&course.teacher),
                    download::sanitize_filename(&course.name),
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
        let course = self
            .courses
            .get(course_index)
            .ok_or_else(|| anyhow::anyhow!("Course index {} out of range", course_index + 1))?;

        let mut sorted_videos = course.videos.clone();
        sorted_videos.sort_by_key(|v| v.view_num);

        let start_idx = start.saturating_sub(1); // 1-indexed
        let end_idx = end.min(sorted_videos.len());

        if start_idx >= end_idx {
            return Err(anyhow::anyhow!(
                "Invalid range: {}-{} (total: {} videos)",
                start,
                end,
                sorted_videos.len()
            ));
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

            let ext = if video.file_ext.is_empty() {
                "mp4"
            } else {
                &video.file_ext
            };
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
            return Err(anyhow::anyhow!(
                "Invalid lecture range: start ({}) > end ({})",
                start,
                end
            ));
        }

        if self.courses.is_empty() {
            return Err(anyhow::anyhow!("No course data loaded"));
        }

        let (subject_name, teacher, course_name) = {
            let first = &self.courses[0];
            (
                first.subject_name.clone(),
                first.teacher.clone(),
                first.name.clone(),
            )
        };

        let mut all_videos: Vec<VideoInfo> =
            self.courses.iter().flat_map(|c| c.videos.clone()).collect();

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

            let ext = if video.file_ext.is_empty() {
                "mp4"
            } else {
                &video.file_ext
            };
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
        self.download_lecture_range(1, total, only_recordings, output_dir)
            .await
    }

    /// Set course ID.
    pub fn set_course_id(&mut self, id: String) {
        self.course_id = Some(id);
    }
}
