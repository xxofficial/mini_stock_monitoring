use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use semver::Version;
use serde::Deserialize;

pub const GITHUB_OWNER: &str = "xxofficial";
pub const GITHUB_REPO: &str = "mini_stock_monitoring";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub html_url: String,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: String,
    pub title: String,
    pub changelog: String,
    pub asset_name: String,
    pub download_url: String,
    pub size: u64,
    pub html_url: String,
}

#[derive(Debug, Clone)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate {
        checked_at: Instant,
    },
    Available(UpdateInfo),
    Downloading {
        info: UpdateInfo,
        downloaded: u64,
        total: u64,
    },
    ReadyToInstall {
        info: UpdateInfo,
        installer_path: PathBuf,
    },
    Failed(String),
}

impl Default for UpdateState {
    fn default() -> Self {
        Self::Idle
    }
}

pub struct UpdateManager {
    state: Arc<Mutex<UpdateState>>,
    cancel_download: Arc<AtomicBool>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl UpdateManager {
    pub fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            state: Arc::new(Mutex::new(UpdateState::Idle)),
            cancel_download: Arc::new(AtomicBool::new(false)),
            repaint,
        }
    }

    pub fn state(&self) -> UpdateState {
        self.state.lock().map(|s| s.clone()).unwrap_or(UpdateState::Idle)
    }

    pub fn check_for_updates(&self, mirror: Option<String>) {
        let state_arc = Arc::clone(&self.state);
        let repaint = Arc::clone(&self.repaint);

        {
            if let Ok(mut state) = state_arc.lock() {
                if matches!(*state, UpdateState::Checking | UpdateState::Downloading { .. }) {
                    return;
                }
                *state = UpdateState::Checking;
            }
        }
        repaint();

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    if let Ok(mut state) = state_arc.lock() {
                        *state = UpdateState::Failed(format!("无法初始化异步运行时: {e}"));
                    }
                    repaint();
                    return;
                }
            };

            rt.block_on(async move {
                let result = fetch_latest_release(mirror.as_deref()).await;
                if let Ok(mut state) = state_arc.lock() {
                    match result {
                        Ok(Some(info)) => {
                            *state = UpdateState::Available(info);
                        }
                        Ok(None) => {
                            *state = UpdateState::UpToDate {
                                checked_at: Instant::now(),
                            };
                        }
                        Err(err) => {
                            *state = UpdateState::Failed(err);
                        }
                    }
                }
                repaint();
            });
        });
    }

    pub fn start_download(&self, info: UpdateInfo, mirror: Option<String>) {
        let state_arc = Arc::clone(&self.state);
        let cancel_flag = Arc::clone(&self.cancel_download);
        let repaint = Arc::clone(&self.repaint);

        cancel_flag.store(false, Ordering::Relaxed);

        {
            if let Ok(mut state) = state_arc.lock() {
                *state = UpdateState::Downloading {
                    info: info.clone(),
                    downloaded: 0,
                    total: info.size,
                };
            }
        }
        repaint();

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    if let Ok(mut state) = state_arc.lock() {
                        *state = UpdateState::Failed(format!("无法初始化异步运行时: {e}"));
                    }
                    repaint();
                    return;
                }
            };

            rt.block_on(async move {
                let download_url = apply_mirror(&info.download_url, mirror.as_deref());
                let result = download_file(
                    &download_url,
                    &info.asset_name,
                    state_arc.clone(),
                    cancel_flag,
                    repaint.clone(),
                    info.clone(),
                )
                .await;

                if let Ok(mut state) = state_arc.lock() {
                    match result {
                        Ok(path) => {
                            *state = UpdateState::ReadyToInstall {
                                info,
                                installer_path: path,
                            };
                        }
                        Err(err) => {
                            *state = UpdateState::Failed(err);
                        }
                    }
                }
                repaint();
            });
        });
    }

    pub fn cancel_download(&self) {
        self.cancel_download.store(true, Ordering::Relaxed);
        if let Ok(mut state) = self.state.lock() {
            *state = UpdateState::Idle;
        }
        (self.repaint)();
    }

    pub fn install_and_exit(installer_path: &PathBuf) -> Result<(), String> {
        if !installer_path.exists() {
            return Err("安装包文件不存在，请重新下载".into());
        }

        let mut command = std::process::Command::new(installer_path);
        command.arg("/CLOSEAPPLICATIONS");

        match command.spawn() {
            Ok(_) => {
                std::process::exit(0);
            }
            Err(e) => Err(format!("启动安装程序失败: {e}")),
        }
    }
}

fn apply_mirror(url: &str, mirror: Option<&str>) -> String {
    if let Some(mirror) = mirror.map(str::trim).filter(|s| !s.is_empty()) {
        let mirror = mirror.trim_end_matches('/');
        format!("{mirror}/{url}")
    } else {
        url.to_string()
    }
}

async fn fetch_latest_release(mirror: Option<&str>) -> Result<Option<UpdateInfo>, String> {
    let api_url = format!(
        "https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest"
    );
    let target_url = apply_mirror(&api_url, mirror);

    let client = reqwest::Client::builder()
        .user_agent(format!("mini-stock-monitor/{CURRENT_VERSION}"))
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("创建网络请求失败: {e}"))?;

    let response = client
        .get(&target_url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("连接 GitHub 更新服务器失败: {e}"))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    if !response.status().is_success() {
        return Err(format!("检查更新失败，服务器返回 HTTP {}", response.status()));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("读取更新信息失败: {e}"))?;

    let release: GithubRelease = serde_json::from_slice(&bytes)
        .map_err(|e| format!("解析更新信息失败: {e}"))?;

    let remote_ver_str = release.tag_name.trim_start_matches(['v', 'V']);
    let remote_ver = Version::parse(remote_ver_str)
        .map_err(|e| format!("远端版本号格式不合法 ({remote_ver_str}): {e}"))?;
    let local_ver = Version::parse(CURRENT_VERSION)
        .map_err(|e| format!("本地版本号格式不合法 ({CURRENT_VERSION}): {e}"))?;

    if remote_ver <= local_ver {
        return Ok(None);
    }

    let asset = select_windows_asset(&release.assets).ok_or_else(|| {
        format!("新版本 {remote_ver_str} 已发布，但未找到对应的 Windows 安装包。")
    })?;

    Ok(Some(UpdateInfo {
        version: remote_ver_str.to_string(),
        title: release.name.unwrap_or_else(|| format!("v{remote_ver_str}")),
        changelog: release.body.unwrap_or_default(),
        asset_name: asset.name.clone(),
        download_url: asset.browser_download_url.clone(),
        size: asset.size,
        html_url: release.html_url,
    }))
}

pub fn select_windows_asset<'a>(assets: &'a [ReleaseAsset]) -> Option<&'a ReleaseAsset> {
    let mut candidate: Option<&ReleaseAsset> = None;
    for asset in assets {
        let name_lower = asset.name.to_lowercase();
        if name_lower.ends_with(".exe") {
            if name_lower.contains("setup") || name_lower.contains("windows-x64") {
                return Some(asset);
            } else if candidate.is_none() {
                candidate = Some(asset);
            }
        }
    }
    candidate
}

async fn download_file(
    url: &str,
    filename: &str,
    state_arc: Arc<Mutex<UpdateState>>,
    cancel_flag: Arc<AtomicBool>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    info: UpdateInfo,
) -> Result<PathBuf, String> {
    let client = reqwest::Client::builder()
        .user_agent(format!("mini-stock-monitor/{CURRENT_VERSION}"))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("创建网络请求失败: {e}"))?;

    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("发起下载请求失败: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("下载安装包失败，服务器返回 HTTP {}", response.status()));
    }

    let total_size = response.content_length().unwrap_or(info.size);

    let temp_dir = std::env::temp_dir();
    let temp_path = temp_dir.join(format!("{}-{filename}", std::process::id()));

    let mut file = tokio::fs::File::create(&temp_path)
        .await
        .map_err(|e| format!("无法创建临时文件: {e}"))?;

    let mut downloaded = 0u64;
    let mut last_repaint = Instant::now();

    use tokio::io::AsyncWriteExt;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("下载数据流中断: {e}"))?
    {
        if cancel_flag.load(Ordering::Relaxed) {
            drop(file);
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err("用户已取消下载".into());
        }

        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入文件失败: {e}"))?;

        downloaded += chunk.len() as u64;

        if last_repaint.elapsed() >= Duration::from_millis(80) || downloaded >= total_size {
            last_repaint = Instant::now();
            if let Ok(mut state) = state_arc.lock() {
                *state = UpdateState::Downloading {
                    info: info.clone(),
                    downloaded,
                    total: total_size,
                };
            }
            repaint();
        }
    }

    file.flush()
        .await
        .map_err(|e| format!("完成文件写入失败: {e}"))?;

    Ok(temp_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_compare() {
        let v1 = Version::parse("0.1.0").unwrap();
        let v2 = Version::parse("0.1.1").unwrap();
        let v10 = Version::parse("0.1.10").unwrap();

        assert!(v2 > v1);
        assert!(v10 > v2);
    }

    #[test]
    fn test_apply_mirror() {
        let url = "https://github.com/xxofficial/mini_stock_monitoring/releases/download/v0.1.0/setup.exe";
        assert_eq!(apply_mirror(url, None), url);
        assert_eq!(
            apply_mirror(url, Some("https://ghproxy.net/")),
            format!("https://ghproxy.net/{url}")
        );
    }

    #[test]
    fn test_select_windows_asset() {
        let assets = vec![
            ReleaseAsset {
                name: "MiniStockMonitor-windows-x64.zip".into(),
                browser_download_url: "https://example.com/zip".into(),
                size: 1000,
            },
            ReleaseAsset {
                name: "微行情-0.2.0-windows-x64-setup.exe".into(),
                browser_download_url: "https://example.com/setup".into(),
                size: 2000,
            },
        ];

        let selected = select_windows_asset(&assets).unwrap();
        assert_eq!(selected.name, "微行情-0.2.0-windows-x64-setup.exe");
        assert_eq!(selected.size, 2000);
    }

    #[test]
    fn test_release_json_parse() {
        let json = r#"{
            "tag_name": "v0.2.0",
            "name": "Release v0.2.0",
            "body": "Fixes and improvements",
            "html_url": "https://github.com/xxofficial/mini_stock_monitoring/releases/tag/v0.2.0",
            "assets": [
                {
                    "name": "微行情-0.2.0-windows-x64-setup.exe",
                    "browser_download_url": "https://github.com/xxofficial/mini_stock_monitoring/releases/download/v0.2.0/setup.exe",
                    "size": 3145728
                }
            ]
        }"#;

        let release: GithubRelease = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v0.2.0");
        assert_eq!(release.assets.len(), 1);
        let asset = select_windows_asset(&release.assets).unwrap();
        assert_eq!(asset.size, 3145728);
    }
}
