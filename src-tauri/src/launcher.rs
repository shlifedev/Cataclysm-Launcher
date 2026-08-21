use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use chrono::Utc;
use futures_util::StreamExt;
use keyring::{Entry, Error as KeyringError};
use quick_xml::{events::Event, Reader};
use reqwest::{header, Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

pub struct AppState {
    pub http: reqwest::Client,
    pub webdav_http: reqwest::Client,
    pub webdav_password: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum GameId {
    Dda,
    Bn,
}

impl GameId {
    fn key(self) -> &'static str {
        match self {
            Self::Dda => "dda",
            Self::Bn => "bn",
        }
    }

    fn repository(self) -> &'static str {
        match self {
            Self::Dda => "CleverRaven/Cataclysm-DDA",
            Self::Bn => "cataclysmbnteam/Cataclysm-BN",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseAsset {
    id: u64,
    name: String,
    size: u64,
    url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    id: u64,
    tag: String,
    name: String,
    prerelease: bool,
    published_at: String,
    body: Option<String>,
    recommended_asset: Option<ReleaseAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleasePage {
    game: GameId,
    page: u16,
    releases: Vec<Release>,
    has_more: bool,
    from_cache: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedReleasePage {
    etag: Option<String>,
    page: ReleasePage,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    id: u64,
    name: String,
    size: u64,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    id: u64,
    tag_name: String,
    name: Option<String>,
    prerelease: bool,
    published_at: Option<String>,
    body: Option<String>,
    assets: Vec<GithubAsset>,
}

impl From<(GameId, GithubRelease)> for Release {
    fn from((game, release): (GameId, GithubRelease)) -> Self {
        let assets = release
            .assets
            .iter()
            .map(|asset| ReleaseAsset {
                id: asset.id,
                name: asset.name.clone(),
                size: asset.size,
                url: asset.browser_download_url.clone(),
            })
            .collect::<Vec<_>>();
        Self {
            id: release.id,
            tag: release.tag_name.clone(),
            name: release.name.unwrap_or(release.tag_name),
            prerelease: release.prerelease,
            published_at: release.published_at.unwrap_or_default(),
            body: release.body,
            recommended_asset: recommend_asset(game, &assets),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRecord {
    id: String,
    game: GameId,
    release_id: u64,
    tag: String,
    name: String,
    asset_name: String,
    installed_at: String,
    install_dir: String,
    user_dir: String,
    executable_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRecord {
    id: String,
    installation_id: String,
    game: GameId,
    release_id: u64,
    tag: String,
    created_at: String,
    archive_path: String,
    size: u64,
    contents: Vec<String>,
    schema_version: u8,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackupManifest {
    schema_version: u8,
    installation_id: String,
    game: GameId,
    release_id: u64,
    tag: String,
    created_at: String,
    contents: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavConnection {
    endpoint: String,
    username: String,
    root_folder: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavConnectionInput {
    endpoint: String,
    username: String,
    password: String,
    root_folder: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteBackupRecord {
    file_name: String,
    game: GameId,
    size: u64,
    modified_at: Option<String>,
}

#[derive(Debug, Default)]
struct WebDavListingItem {
    href: String,
    size: Option<u64>,
    modified_at: Option<String>,
    is_collection: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallationLocation {
    Install,
    Config,
    Save,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallProgress {
    release_id: u64,
    stage: &'static str,
    received_bytes: u64,
    total_bytes: u64,
    message: String,
}

type AppResult<T> = Result<T, String>;

const WEBDAV_PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:resourcetype/>
    <d:getcontentlength/>
    <d:getlastmodified/>
  </d:prop>
</d:propfind>"#;

fn app_root(app: &AppHandle) -> AppResult<PathBuf> {
    app.path()
        .app_data_dir()
        .map_err(|error| format!("런처 데이터 폴더를 찾지 못했습니다: {error}"))
}

fn cache_file(app: &AppHandle, game: GameId, page: u16) -> AppResult<PathBuf> {
    Ok(app_root(app)?
        .join("cache")
        .join(format!("{}-{page}.json", game.key())))
}

fn installations_file(app: &AppHandle) -> AppResult<PathBuf> {
    Ok(app_root(app)?.join("installations.json"))
}

fn game_user_dir(app: &AppHandle, game: GameId) -> AppResult<PathBuf> {
    Ok(app_root(app)?.join("userdata").join(game.key()))
}

fn backups_file(app: &AppHandle) -> AppResult<PathBuf> {
    Ok(app_root(app)?.join("backups.json"))
}

fn webdav_config_file(app: &AppHandle) -> AppResult<PathBuf> {
    Ok(app_root(app)?.join("cloud").join("webdav.json"))
}

fn read_webdav_connection(app: &AppHandle) -> AppResult<Option<WebDavConnection>> {
    let path = webdav_config_file(app)?;
    if !path.exists() {
        return Ok(None);
    }
    serde_json::from_slice(
        &fs::read(path).map_err(|error| format!("WebDAV 설정을 읽지 못했습니다: {error}"))?,
    )
    .map(Some)
    .map_err(|error| format!("WebDAV 설정 형식이 올바르지 않습니다: {error}"))
}

fn persist_webdav_connection(app: &AppHandle, connection: &WebDavConnection) -> AppResult<()> {
    write_json(&webdav_config_file(app)?, connection)
}

fn webdav_keychain_entry() -> AppResult<Entry> {
    Entry::new("gg.cataclysm.hub.webdav", "default")
        .map_err(|error| format!("시스템 자격 증명 저장소에 접근하지 못했습니다: {error}"))
}

fn read_webdav_password(state: &AppState) -> AppResult<String> {
    let mut cached_password = state
        .webdav_password
        .lock()
        .map_err(|_| "WebDAV 비밀번호 캐시에 접근하지 못했습니다.".to_string())?;
    if let Some(password) = cached_password.as_ref() {
        return Ok(password.clone());
    }

    let password = webdav_keychain_entry()?
        .get_password()
        .map_err(webdav_keychain_error)?;
    *cached_password = Some(password.clone());
    Ok(password)
}

fn webdav_keychain_error(error: KeyringError) -> String {
    match error {
        KeyringError::NoEntry => "WebDAV 비밀번호를 찾지 못했습니다. 클라우드 연결을 다시 설정해 주세요.".into(),
        KeyringError::NoStorageAccess(detail) => format!(
            "시스템 자격 증명 저장소에 접근할 수 없습니다. 저장소가 잠겨 있거나 앱 접근이 거부됐을 수 있습니다: {detail}"
        ),
        KeyringError::PlatformFailure(detail) => {
            format!("시스템 자격 증명 저장소 작업에 실패했습니다: {detail}")
        }
        other => format!("WebDAV 자격 증명을 처리하지 못했습니다: {other}"),
    }
}

fn store_webdav_password(state: &AppState, password: &str) -> AppResult<()> {
    webdav_keychain_entry()?
        .set_password(password)
        .map_err(webdav_keychain_error)?;
    *state
        .webdav_password
        .lock()
        .map_err(|_| "WebDAV 비밀번호 캐시에 접근하지 못했습니다.".to_string())? =
        Some(password.to_string());
    Ok(())
}

fn clear_webdav_password(state: &AppState) -> AppResult<()> {
    *state
        .webdav_password
        .lock()
        .map_err(|_| "WebDAV 비밀번호 캐시에 접근하지 못했습니다.".to_string())? = None;
    Ok(())
}

fn is_safe_remote_file_name(value: &str) -> bool {
    !value.is_empty()
        && value.ends_with(".zip")
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

fn normalize_webdav_connection(
    input: WebDavConnectionInput,
) -> AppResult<(WebDavConnection, String)> {
    let endpoint = input.endpoint.trim();
    let username = input.username.trim();
    let password = input.password;
    if username.is_empty() || password.is_empty() {
        return Err("WebDAV 사용자명과 앱 비밀번호를 모두 입력해 주세요.".into());
    }
    let mut endpoint = Url::parse(endpoint).map_err(|_| "WebDAV 서버 URL이 올바르지 않습니다.")?;
    if endpoint.scheme() != "https" && endpoint.scheme() != "http" {
        return Err("WebDAV 서버 URL은 HTTP 또는 HTTPS여야 합니다.".into());
    }
    if endpoint.query().is_some() || endpoint.fragment().is_some() {
        return Err("WebDAV 서버 URL에는 쿼리나 앵커를 포함할 수 없습니다.".into());
    }
    if !endpoint.path().ends_with('/') {
        let path = format!("{}/", endpoint.path());
        endpoint.set_path(&path);
    }
    let root_folder = input.root_folder.trim().trim_matches('/');
    let root_folder = if root_folder.is_empty() {
        "CataclysmHub"
    } else {
        root_folder
    };
    if root_folder.split('/').any(|segment| {
        segment.is_empty()
            || segment == "."
            || segment == ".."
            || !segment.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
    }) {
        return Err("원격 폴더는 영문·숫자·하이픈·밑줄과 슬래시만 사용할 수 있습니다.".into());
    }
    Ok((
        WebDavConnection {
            endpoint: endpoint.to_string(),
            username: username.to_string(),
            root_folder: root_folder.to_string(),
        },
        password,
    ))
}

fn webdav_root_url(connection: &WebDavConnection) -> AppResult<Url> {
    Url::parse(&connection.endpoint)
        .map_err(|_| "저장된 WebDAV 서버 URL이 올바르지 않습니다.".to_string())?
        .join(&format!("{}/", connection.root_folder))
        .map_err(|_| "WebDAV 원격 폴더 URL을 만들지 못했습니다.".to_string())
}

fn webdav_game_url(connection: &WebDavConnection, game: GameId) -> AppResult<Url> {
    webdav_root_url(connection)?
        .join(&format!("{}/", game.key()))
        .map_err(|_| "WebDAV 게임 폴더 URL을 만들지 못했습니다.".to_string())
}

fn read_cached_page(app: &AppHandle, game: GameId, page: u16) -> Option<CachedReleasePage> {
    let path = cache_file(app, game, page).ok()?;
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> AppResult<()> {
    let parent = path.parent().ok_or("저장 경로가 올바르지 않습니다")?;
    fs::create_dir_all(parent).map_err(|error| format!("폴더를 만들지 못했습니다: {error}"))?;
    let pending = parent.join(format!(".pending-{}", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    fs::write(&pending, bytes).map_err(|error| format!("데이터를 저장하지 못했습니다: {error}"))?;
    let result = replace_file(&pending, path)
        .map_err(|error| format!("데이터를 확정하지 못했습니다: {error}"));
    if result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    result
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::{iter, os::windows::ffi::OsStrExt};
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: Both pointers reference null-terminated UTF-16 buffers for the duration of the call.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn read_installations(app: &AppHandle) -> AppResult<Vec<InstallRecord>> {
    let path = installations_file(app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(
        &fs::read(path).map_err(|error| format!("설치 목록을 읽지 못했습니다: {error}"))?,
    )
    .map_err(|error| format!("설치 목록 형식이 올바르지 않습니다: {error}"))
}

fn save_installations(app: &AppHandle, records: &[InstallRecord]) -> AppResult<()> {
    write_json(&installations_file(app)?, records)
}

fn active_installations(records: &[InstallRecord]) -> Vec<InstallRecord> {
    [GameId::Dda, GameId::Bn]
        .into_iter()
        .filter_map(|game| {
            records
                .iter()
                .filter(|record| record.game == game)
                .max_by(|left, right| left.installed_at.cmp(&right.installed_at))
                .cloned()
        })
        .collect()
}

fn activate_cached_installation(
    records: &mut [InstallRecord],
    game: GameId,
    release_id: u64,
    activated_at: String,
) -> Option<InstallRecord> {
    let record = records
        .iter_mut()
        .find(|record| record.game == game && record.release_id == release_id)?;
    record.installed_at = activated_at;
    Some(record.clone())
}

fn read_backups(app: &AppHandle) -> AppResult<Vec<BackupRecord>> {
    let path = backups_file(app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(
        &fs::read(path).map_err(|error| format!("백업 목록을 읽지 못했습니다: {error}"))?,
    )
    .map_err(|error| format!("백업 목록 형식이 올바르지 않습니다: {error}"))
}

fn save_backups(app: &AppHandle, records: &[BackupRecord]) -> AppResult<()> {
    write_json(&backups_file(app)?, records)
}

fn installation_record(app: &AppHandle, installation_id: &str) -> AppResult<InstallRecord> {
    read_installations(app)?
        .into_iter()
        .find(|record| record.id == installation_id)
        .ok_or_else(|| "설치된 버전을 찾지 못했습니다.".to_string())
}

fn backup_record(app: &AppHandle, backup_id: &str) -> AppResult<BackupRecord> {
    read_backups(app)?
        .into_iter()
        .find(|record| record.id == backup_id)
        .ok_or_else(|| "백업 파일을 찾지 못했습니다.".to_string())
}

fn api_error(status: StatusCode) -> String {
    if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
        "GitHub API 요청 한도에 도달했습니다. 잠시 후 다시 시도해 주세요.".to_string()
    } else {
        format!(
            "GitHub 릴리즈 정보를 가져오지 못했습니다 (HTTP {}).",
            status.as_u16()
        )
    }
}

fn webdav_error(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            "WebDAV 인증에 실패했습니다. 사용자명 또는 앱 비밀번호를 확인해 주세요.".into()
        }
        StatusCode::NOT_FOUND => "WebDAV 서버 또는 지정한 경로를 찾지 못했습니다.".into(),
        StatusCode::METHOD_NOT_ALLOWED => {
            "WebDAV 폴더 조회가 거부되었습니다 (HTTP 405). 웹 관리 화면 주소가 아니라 PROPFIND를 지원하는 실제 WebDAV 서버 URL인지 확인해 주세요.".into()
        }
        _ => format!(
            "WebDAV 서버 요청을 완료하지 못했습니다 (HTTP {}).",
            status.as_u16()
        ),
    }
}

fn dav_method(value: &'static [u8]) -> AppResult<Method> {
    Method::from_bytes(value).map_err(|_| "WebDAV 요청 방식을 만들지 못했습니다.".to_string())
}

fn alternate_collection_url(mut url: Url) -> Option<Url> {
    let path = url.path();
    if path == "/" {
        return None;
    }
    let alternate = if path.ends_with('/') {
        path.trim_end_matches('/').to_string()
    } else {
        format!("{path}/")
    };
    url.set_path(&alternate);
    Some(url)
}

async fn send_webdav_propfind(
    state: &AppState,
    url: Url,
    username: &str,
    password: &str,
    depth: &'static str,
) -> AppResult<reqwest::Response> {
    state
        .webdav_http
        .request(dav_method(b"PROPFIND")?, url)
        .header("Depth", depth)
        .header(header::ACCEPT, "application/xml, text/xml")
        .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
        .basic_auth(username, Some(password))
        .body(WEBDAV_PROPFIND_BODY)
        .send()
        .await
        .map_err(|error| format!("WebDAV 서버에 연결하지 못했습니다: {error}"))
}

async fn webdav_propfind(
    state: &AppState,
    url: Url,
    username: &str,
    password: &str,
    depth: &'static str,
) -> AppResult<reqwest::Response> {
    let alternate_url = alternate_collection_url(url.clone());
    let response = send_webdav_propfind(state, url, username, password, depth).await?;
    if matches!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_FOUND
    ) {
        if let Some(alternate_url) = alternate_url {
            let alternate =
                send_webdav_propfind(state, alternate_url, username, password, depth).await?;
            if alternate.status().as_u16() == 207 {
                return Ok(alternate);
            }
        }
    }
    Ok(response)
}

async fn webdav_listing(
    state: &AppState,
    url: Url,
    username: &str,
    password: &str,
    depth: &'static str,
) -> AppResult<Vec<WebDavListingItem>> {
    let response = webdav_propfind(state, url, username, password, depth).await?;
    if response.status().as_u16() != 207 {
        return Err(webdav_error(response.status()));
    }
    parse_webdav_listing(
        &response
            .text()
            .await
            .map_err(|error| format!("WebDAV 응답을 읽지 못했습니다: {error}"))?,
    )
}

async fn verify_webdav_collection(
    state: &AppState,
    url: Url,
    username: &str,
    password: &str,
) -> AppResult<()> {
    let listing = webdav_listing(state, url, username, password, "0").await?;
    if listing.iter().any(|item| item.is_collection) {
        Ok(())
    } else {
        Err("지정한 WebDAV 경로가 폴더 컬렉션이 아닙니다.".into())
    }
}

async fn check_webdav_connection(
    state: &AppState,
    connection: &WebDavConnection,
    password: &str,
) -> AppResult<()> {
    let endpoint = Url::parse(&connection.endpoint)
        .map_err(|_| "저장된 WebDAV 서버 URL이 올바르지 않습니다.")?;
    verify_webdav_collection(state, endpoint, &connection.username, password).await
}

async fn ensure_webdav_collection(
    state: &AppState,
    url: Url,
    username: &str,
    password: &str,
) -> AppResult<()> {
    let response = state
        .webdav_http
        .request(dav_method(b"MKCOL")?, url.clone())
        .basic_auth(username, Some(password))
        .send()
        .await
        .map_err(|error| format!("WebDAV 폴더를 만들지 못했습니다: {error}"))?;
    match response.status() {
        StatusCode::CREATED | StatusCode::METHOD_NOT_ALLOWED => {
            verify_webdav_collection(state, url, username, password).await
        }
        status => Err(webdav_error(status)),
    }
}

async fn ensure_webdav_directories(
    state: &AppState,
    connection: &WebDavConnection,
    password: &str,
) -> AppResult<()> {
    let endpoint = Url::parse(&connection.endpoint)
        .map_err(|_| "저장된 WebDAV 서버 URL이 올바르지 않습니다.")?;
    let mut folder_path = String::new();
    for segment in connection.root_folder.split('/') {
        folder_path.push_str(segment);
        folder_path.push('/');
        let url = endpoint
            .join(&folder_path)
            .map_err(|_| "WebDAV 원격 폴더 URL을 만들지 못했습니다.")?;
        ensure_webdav_collection(state, url, &connection.username, password).await?;
    }
    for game in [GameId::Dda, GameId::Bn] {
        ensure_webdav_collection(
            state,
            webdav_game_url(connection, game)?,
            &connection.username,
            password,
        )
        .await?;
        webdav_listing(
            state,
            webdav_game_url(connection, game)?,
            &connection.username,
            password,
            "1",
        )
        .await?;
    }
    Ok(())
}

fn xml_local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|character| *character == b':') {
        Some(position) => &name[position + 1..],
        None => name,
    }
}

fn parse_webdav_listing(xml: &str) -> AppResult<Vec<WebDavListingItem>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut current: Option<WebDavListingItem> = None;
    let mut text_target: Option<&'static str> = None;
    let mut items = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => match xml_local_name(event.name().as_ref()) {
                b"response" => current = Some(WebDavListingItem::default()),
                b"href" if current.is_some() => text_target = Some("href"),
                b"getcontentlength" if current.is_some() => text_target = Some("size"),
                b"getlastmodified" if current.is_some() => text_target = Some("modified"),
                b"collection" if current.is_some() => {
                    if let Some(item) = current.as_mut() {
                        item.is_collection = true;
                    }
                }
                _ => {}
            },
            Ok(Event::Text(event)) => {
                if let (Some(item), Some(target)) = (current.as_mut(), text_target) {
                    let value = String::from_utf8_lossy(event.as_ref()).trim().to_string();
                    match target {
                        "href" => item.href = value,
                        "size" => item.size = value.parse().ok(),
                        "modified" => item.modified_at = Some(value),
                        _ => {}
                    }
                }
            }
            Ok(Event::CData(event)) => {
                if let (Some(item), Some(target)) = (current.as_mut(), text_target) {
                    let value = String::from_utf8_lossy(event.as_ref()).trim().to_string();
                    match target {
                        "href" => item.href = value,
                        "size" => item.size = value.parse().ok(),
                        "modified" => item.modified_at = Some(value),
                        _ => {}
                    }
                }
            }
            Ok(Event::Empty(event))
                if xml_local_name(event.name().as_ref()) == b"collection" && current.is_some() =>
            {
                if let Some(item) = current.as_mut() {
                    item.is_collection = true;
                }
            }
            Ok(Event::End(event)) => match xml_local_name(event.name().as_ref()) {
                b"href" | b"getcontentlength" | b"getlastmodified" => text_target = None,
                b"response" => {
                    if let Some(item) = current.take() {
                        items.push(item);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("WebDAV 응답을 해석하지 못했습니다: {error}")),
            _ => {}
        }
        buffer.clear();
    }
    Ok(items)
}

fn remote_file_name_from_href(href: &str) -> Option<&str> {
    if href
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return None;
    }
    let path = href.trim_end_matches('/');
    path.rsplit('/')
        .next()
        .filter(|name| is_safe_remote_file_name(name))
}

fn webdav_backup_url(
    connection: &WebDavConnection,
    game: GameId,
    file_name: &str,
) -> AppResult<Url> {
    if !is_safe_remote_file_name(file_name) {
        return Err("WebDAV 백업 파일 이름이 올바르지 않습니다.".into());
    }
    webdav_game_url(connection, game)?
        .join(file_name)
        .map_err(|_| "WebDAV 백업 파일 URL을 만들지 못했습니다.".to_string())
}

async fn github_releases_page(
    state: &AppState,
    game: GameId,
    page: u16,
    etag: Option<&str>,
) -> AppResult<reqwest::Response> {
    let url = format!(
        "https://api.github.com/repos/{}/releases?per_page=100&page={page}",
        game.repository()
    );
    let mut request = state
        .http
        .get(url)
        .header(header::ACCEPT, "application/vnd.github+json");
    if let Some(value) = etag {
        request = request.header(header::IF_NONE_MATCH, value);
    }
    request
        .send()
        .await
        .map_err(|error| format!("GitHub에 연결하지 못했습니다: {error}"))
}

#[tauri::command]
pub async fn fetch_releases(
    app: AppHandle,
    state: State<'_, AppState>,
    game: GameId,
    page: u16,
    refresh: Option<bool>,
) -> AppResult<ReleasePage> {
    let _ = refresh;
    if !(1..=10).contains(&page) {
        return Err("최근 1,000개 릴리즈까지만 탐색할 수 있습니다.".into());
    }

    let cached = read_cached_page(&app, game, page);
    let response = github_releases_page(
        &state,
        game,
        page,
        cached.as_ref().and_then(|item| item.etag.as_deref()),
    )
    .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            if let Some(mut fallback) = cached.map(|item| item.page) {
                fallback.from_cache = true;
                return Ok(fallback);
            }
            return Err(error);
        }
    };

    if response.status() == StatusCode::NOT_MODIFIED {
        if let Some(mut cached) = cached.as_ref().map(|item| item.page.clone()) {
            cached.from_cache = true;
            return Ok(cached);
        }
    }
    if !response.status().is_success() {
        if let Some(mut fallback) = cached.as_ref().map(|item| item.page.clone()) {
            fallback.from_cache = true;
            return Ok(fallback);
        }
        return Err(api_error(response.status()));
    }

    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let github_releases = response
        .json::<Vec<GithubRelease>>()
        .await
        .map_err(|error| format!("GitHub 응답을 해석하지 못했습니다: {error}"))?;
    let has_more = github_releases.len() == 100 && page < 10;
    let result = ReleasePage {
        game,
        page,
        releases: github_releases
            .into_iter()
            .map(|release| (game, release).into())
            .collect(),
        has_more,
        from_cache: false,
    };
    write_json(
        &cache_file(&app, game, page)?,
        &CachedReleasePage {
            etag,
            page: result.clone(),
        },
    )?;
    Ok(result)
}

async fn fetch_release(state: &AppState, game: GameId, release_id: u64) -> AppResult<Release> {
    let url = format!(
        "https://api.github.com/repos/{}/releases/{release_id}",
        game.repository()
    );
    let response = state
        .http
        .get(url)
        .header(header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("GitHub에 연결하지 못했습니다: {error}"))?;
    if !response.status().is_success() {
        return Err(api_error(response.status()));
    }
    let github = response
        .json::<GithubRelease>()
        .await
        .map_err(|error| format!("릴리즈 응답을 해석하지 못했습니다: {error}"))?;
    Ok((game, github).into())
}

fn recommend_asset_for(
    game: GameId,
    assets: &[ReleaseAsset],
    operating_system: &str,
    architecture: &str,
) -> Option<ReleaseAsset> {
    assets
        .iter()
        .filter_map(|asset| {
            let name = asset.name.to_ascii_lowercase();
            let is_arm = name.contains("-arm-") || name.contains("_arm_") || name.contains("arm64");
            let is_x64 =
                name.contains("-x64-") || name.contains("_x64_") || name.contains("x86_64");
            let is_universal = name.contains("universal");
            let score = match operating_system {
                "macos" => {
                    if !name.ends_with(".dmg")
                        || !name.contains("osx")
                        || name.contains("no-soundpack")
                        || name.contains("curses")
                        || name.contains("terminal")
                    {
                        return None;
                    }
                    let has_tiles = match game {
                        GameId::Dda => name.contains("with-graphics") || name.contains("tiles"),
                        GameId::Bn => name.contains("tiles"),
                    };
                    if !has_tiles
                        || (architecture == "aarch64" && is_x64 && !is_universal)
                        || (architecture == "x86_64" && is_arm && !is_universal)
                    {
                        return None;
                    }
                    100 + i32::from(is_universal) * 30
                        + i32::from(architecture == "aarch64" && is_arm) * 20
                        + i32::from(architecture == "x86_64" && is_x64) * 20
                        + i32::from(name.contains("with-graphics")) * 5
                }
                "windows" => {
                    if architecture != "x86_64"
                        || !name.ends_with(".zip")
                        || !name.contains("windows")
                        || !is_x64
                        || name.contains("no-soundpack")
                        || name.contains("pdb")
                        || name.contains("curses")
                        || name.contains("terminal")
                    {
                        return None;
                    }
                    let has_tiles = match game {
                        GameId::Dda => name.contains("with-graphics") || name.contains("tiles"),
                        GameId::Bn => name.contains("tiles"),
                    };
                    if !has_tiles {
                        return None;
                    }
                    100 + i32::from(name.contains("and-sounds")) * 30
                        + i32::from(name.contains("msvc")) * 5
                }
                _ => return None,
            };
            Some((score, asset.clone()))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, asset)| asset)
}

fn recommend_asset(game: GameId, assets: &[ReleaseAsset]) -> Option<ReleaseAsset> {
    recommend_asset_for(game, assets, std::env::consts::OS, std::env::consts::ARCH)
}

fn emit_progress(
    app: &AppHandle,
    release_id: u64,
    stage: &'static str,
    received: u64,
    total: u64,
    message: impl Into<String>,
) {
    let _ = app.emit(
        "install-progress",
        InstallProgress {
            release_id,
            stage,
            received_bytes: received,
            total_bytes: total,
            message: message.into(),
        },
    );
}

async fn download_asset(
    app: &AppHandle,
    state: &AppState,
    release_id: u64,
    asset: &ReleaseAsset,
    destination: &Path,
) -> AppResult<()> {
    let response = state
        .http
        .get(&asset.url)
        .send()
        .await
        .map_err(|error| format!("설치 파일을 내려받지 못했습니다: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "설치 파일을 내려받지 못했습니다 (HTTP {}).",
            response.status().as_u16()
        ));
    }
    let parent = destination
        .parent()
        .ok_or("임시 다운로드 경로가 올바르지 않습니다")?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("임시 폴더를 만들지 못했습니다: {error}"))?;
    let mut file = tokio::fs::File::create(destination)
        .await
        .map_err(|error| format!("임시 설치 파일을 만들지 못했습니다: {error}"))?;
    let mut stream = response.bytes_stream();
    let mut received = 0_u64;
    let mut last_reported = 0_u64;
    emit_progress(
        app,
        release_id,
        "downloading",
        0,
        asset.size,
        "설치 파일을 내려받는 중",
    );
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|error| format!("설치 파일 다운로드가 중단되었습니다: {error}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("설치 파일을 저장하지 못했습니다: {error}"))?;
        received += chunk.len() as u64;
        if received.saturating_sub(last_reported) >= 1_048_576 || received == asset.size {
            emit_progress(
                app,
                release_id,
                "downloading",
                received,
                asset.size,
                "설치 파일을 내려받는 중",
            );
            last_reported = received;
        }
    }
    file.flush()
        .await
        .map_err(|error| format!("설치 파일을 마무리하지 못했습니다: {error}"))?;
    if received != asset.size {
        return Err(format!(
            "다운로드 파일 크기가 릴리즈 정보와 다릅니다 ({received} / {}).",
            asset.size
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_command(command: &mut Command, description: &str) -> AppResult<()> {
    let output = command
        .output()
        .map_err(|error| format!("{description} 명령을 실행하지 못했습니다: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() {
        format!("{description}에 실패했습니다.")
    } else {
        format!("{description}에 실패했습니다: {detail}")
    })
}

#[cfg(target_os = "macos")]
fn find_app_bundle(mount: &Path) -> AppResult<PathBuf> {
    fs::read_dir(mount)
        .map_err(|error| format!("DMG 내용을 읽지 못했습니다: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "app"))
        .ok_or_else(|| "DMG에서 게임 앱 번들을 찾지 못했습니다.".into())
}

#[cfg(target_os = "macos")]
fn find_tile_executable(app_bundle: &Path, game: GameId) -> AppResult<PathBuf> {
    let resources = app_bundle.join("Contents").join("Resources");
    let preferred = match game {
        GameId::Dda => "cataclysm-tiles",
        GameId::Bn => "cataclysm-bn-tiles",
    };
    let preferred_path = resources.join(preferred);
    if preferred_path.is_file() {
        return Ok(preferred_path);
    }
    fs::read_dir(&resources)
        .map_err(|error| format!("게임 리소스 폴더를 읽지 못했습니다: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains("tiles"))
        })
        .ok_or_else(|| "게임의 그래픽 실행 파일을 찾지 못했습니다.".into())
}

#[cfg(target_os = "macos")]
fn unpack_dmg(
    app: &AppHandle,
    game: GameId,
    release: &Release,
    asset: &ReleaseAsset,
    dmg: &Path,
) -> AppResult<InstallRecord> {
    let root = app_root(app)?;
    let install_root = root.join("installs").join(game.key());
    let final_dir = install_root.join(release.id.to_string());
    if final_dir.exists() {
        return Err("이 릴리즈는 이미 설치되어 있습니다.".into());
    }
    fs::create_dir_all(&install_root)
        .map_err(|error| format!("설치 폴더를 만들지 못했습니다: {error}"))?;
    let staging = install_root.join(format!(".staging-{}", Uuid::new_v4()));
    let mount = install_root.join(format!(".mount-{}", Uuid::new_v4()));
    fs::create_dir_all(&staging)
        .map_err(|error| format!("임시 설치 폴더를 만들지 못했습니다: {error}"))?;
    fs::create_dir_all(&mount)
        .map_err(|error| format!("임시 마운트 폴더를 만들지 못했습니다: {error}"))?;

    emit_progress(
        app,
        release.id,
        "unpacking",
        0,
        asset.size,
        "DMG를 마운트하는 중",
    );
    let mounted = run_command(
        Command::new("/usr/bin/hdiutil")
            .arg("attach")
            .arg("-nobrowse")
            .arg("-readonly")
            .arg("-noverify")
            .arg("-mountpoint")
            .arg(&mount)
            .arg(dmg),
        "DMG 마운트",
    );
    if let Err(error) = mounted {
        let _ = fs::remove_dir_all(&staging);
        let _ = fs::remove_dir_all(&mount);
        return Err(error);
    }

    let result = (|| -> AppResult<InstallRecord> {
        let source_app = find_app_bundle(&mount)?;
        let app_name = source_app
            .file_name()
            .ok_or("게임 앱 이름을 읽지 못했습니다")?;
        let copied_app = staging.join("app").join(app_name);
        fs::create_dir_all(copied_app.parent().ok_or("설치 경로가 올바르지 않습니다")?)
            .map_err(|error| format!("앱 설치 폴더를 만들지 못했습니다: {error}"))?;
        emit_progress(
            app,
            release.id,
            "unpacking",
            0,
            asset.size,
            "게임 앱을 복사하는 중",
        );
        run_command(
            Command::new("/usr/bin/ditto")
                .arg(&source_app)
                .arg(&copied_app),
            "게임 앱 복사",
        )?;
        let executable = find_tile_executable(&copied_app, game)?;
        fs::rename(&staging, &final_dir)
            .map_err(|error| format!("설치본을 확정하지 못했습니다: {error}"))?;

        let user_dir = game_user_dir(app, game)?;
        fs::create_dir_all(&user_dir)
            .map_err(|error| format!("게임 데이터 폴더를 만들지 못했습니다: {error}"))?;
        let final_executable = final_dir.join(
            executable
                .strip_prefix(&staging)
                .map_err(|_| "실행 파일 경로를 확정하지 못했습니다")?,
        );
        Ok(InstallRecord {
            id: format!("{}-{}", game.key(), release.id),
            game,
            release_id: release.id,
            tag: release.tag.clone(),
            name: release.name.clone(),
            asset_name: asset.name.clone(),
            installed_at: Utc::now().to_rfc3339(),
            install_dir: final_dir.to_string_lossy().to_string(),
            user_dir: user_dir.to_string_lossy().to_string(),
            executable_path: final_executable.to_string_lossy().to_string(),
        })
    })();

    let detach = run_command(
        Command::new("/usr/bin/hdiutil").arg("detach").arg(&mount),
        "DMG 마운트 해제",
    );
    let _ = fs::remove_dir_all(&mount);
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    detach?;
    result
}

#[cfg(any(target_os = "windows", test))]
fn is_safe_windows_archive_path(path: &Path) -> bool {
    const RESERVED_NAMES: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];

    path.components().all(|component| {
        let std::path::Component::Normal(component) = component else {
            return false;
        };
        let name = component.to_string_lossy();
        if name.is_empty()
            || name.ends_with([' ', '.'])
            || name
                .chars()
                .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
        {
            return false;
        }
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        !RESERVED_NAMES.contains(&stem.as_str())
    })
}

#[cfg(any(target_os = "windows", test))]
fn extract_windows_game_zip(archive_path: &Path, destination: &Path) -> AppResult<()> {
    const MAX_UNPACKED_BYTES: u64 = 20 * 1024 * 1024 * 1024;

    let file = fs::File::open(archive_path)
        .map_err(|error| format!("다운로드한 게임 ZIP을 열지 못했습니다: {error}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("다운로드한 파일은 올바른 ZIP이 아닙니다: {error}"))?;
    let unpacked_size = (0..archive.len()).try_fold(0_u64, |total, index| {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("게임 ZIP 항목을 읽지 못했습니다: {error}"))?;
        total
            .checked_add(entry.size())
            .ok_or_else(|| "게임 ZIP의 압축 해제 크기가 너무 큽니다.".to_string())
    })?;
    if unpacked_size > MAX_UNPACKED_BYTES {
        return Err("게임 ZIP의 압축 해제 크기가 안전 제한을 초과합니다.".into());
    }

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("게임 ZIP 항목을 읽지 못했습니다: {error}"))?;
        if entry.is_symlink() {
            return Err("게임 ZIP에 지원하지 않는 심볼릭 링크가 포함되어 있습니다.".into());
        }
        let relative_path = entry
            .enclosed_name()
            .ok_or("게임 ZIP에 안전하지 않은 파일 경로가 포함되어 있습니다.")?;
        if relative_path.as_os_str().is_empty() || !is_safe_windows_archive_path(&relative_path) {
            return Err(
                "게임 ZIP에 Windows에서 안전하지 않은 파일 경로가 포함되어 있습니다.".into(),
            );
        }
        let output_path = destination.join(relative_path);
        if entry.is_dir() {
            fs::create_dir_all(&output_path)
                .map_err(|error| format!("게임 폴더를 만들지 못했습니다: {error}"))?;
            continue;
        }
        let parent = output_path
            .parent()
            .ok_or("게임 파일의 설치 경로가 올바르지 않습니다.")?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("게임 폴더를 만들지 못했습니다: {error}"))?;
        let mut output = fs::File::create(&output_path)
            .map_err(|error| format!("게임 파일을 만들지 못했습니다: {error}"))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|error| format!("게임 파일을 압축 해제하지 못했습니다: {error}"))?;
    }
    Ok(())
}

#[cfg(any(target_os = "windows", test))]
fn windows_executable_priority(game: GameId, file_name: &str) -> Option<u8> {
    let name = file_name.to_ascii_lowercase();
    let candidates: &[&str] = match game {
        GameId::Dda => &["cataclysm-tiles.exe", "cataclysm.exe"],
        GameId::Bn => &[
            "cataclysm-bn-tiles.exe",
            "cataclysm-tiles.exe",
            "cataclysm-bn.exe",
        ],
    };
    if let Some(index) = candidates.iter().position(|candidate| name == *candidate) {
        return Some((candidates.len() - index) as u8 + 10);
    }
    (name.ends_with(".exe")
        && name.contains("cataclysm")
        && (name.contains("tiles") || name == "cataclysm.exe")
        && !name.contains("test"))
    .then_some(1)
}

#[cfg(any(target_os = "windows", test))]
fn find_windows_executable(root: &Path, game: GameId) -> AppResult<PathBuf> {
    let mut directories = vec![root.to_path_buf()];
    let mut best: Option<(u8, PathBuf)> = None;
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("게임 설치 폴더를 읽지 못했습니다: {error}"))?
        {
            let entry = entry.map_err(|error| format!("게임 파일을 읽지 못했습니다: {error}"))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("게임 파일 정보를 읽지 못했습니다: {error}"))?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                directories.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(priority) = windows_executable_priority(game, &name) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|(best_priority, _)| priority > *best_priority)
            {
                best = Some((priority, entry.path()));
            }
        }
    }
    best.map(|(_, path)| path)
        .ok_or_else(|| "게임의 Windows 그래픽 실행 파일을 찾지 못했습니다.".into())
}

#[cfg(target_os = "windows")]
fn unpack_windows_zip(
    app: &AppHandle,
    game: GameId,
    release: &Release,
    asset: &ReleaseAsset,
    archive_path: &Path,
) -> AppResult<InstallRecord> {
    let root = app_root(app)?;
    let install_root = root.join("installs").join(game.key());
    let final_dir = install_root.join(release.id.to_string());
    if final_dir.exists() {
        return Err("이 릴리즈는 이미 설치되어 있습니다.".into());
    }
    fs::create_dir_all(&install_root)
        .map_err(|error| format!("설치 폴더를 만들지 못했습니다: {error}"))?;
    let staging = install_root.join(format!(".staging-{}", Uuid::new_v4()));
    fs::create_dir_all(&staging)
        .map_err(|error| format!("임시 설치 폴더를 만들지 못했습니다: {error}"))?;

    emit_progress(
        app,
        release.id,
        "unpacking",
        0,
        asset.size,
        "게임 ZIP을 압축 해제하는 중",
    );
    let result = (|| -> AppResult<InstallRecord> {
        extract_windows_game_zip(archive_path, &staging)?;
        let executable = find_windows_executable(&staging, game)?;
        fs::rename(&staging, &final_dir)
            .map_err(|error| format!("설치본을 확정하지 못했습니다: {error}"))?;
        let final_executable = final_dir.join(
            executable
                .strip_prefix(&staging)
                .map_err(|_| "실행 파일 경로를 확정하지 못했습니다")?,
        );
        let user_dir = game_user_dir(app, game)?;
        fs::create_dir_all(&user_dir)
            .map_err(|error| format!("게임 데이터 폴더를 만들지 못했습니다: {error}"))?;
        Ok(InstallRecord {
            id: format!("{}-{}", game.key(), release.id),
            game,
            release_id: release.id,
            tag: release.tag.clone(),
            name: release.name.clone(),
            asset_name: asset.name.clone(),
            installed_at: Utc::now().to_rfc3339(),
            install_dir: final_dir.to_string_lossy().to_string(),
            user_dir: user_dir.to_string_lossy().to_string(),
            executable_path: final_executable.to_string_lossy().to_string(),
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(target_os = "macos")]
fn unpack_release_asset(
    app: &AppHandle,
    game: GameId,
    release: &Release,
    asset: &ReleaseAsset,
    archive_path: &Path,
) -> AppResult<InstallRecord> {
    unpack_dmg(app, game, release, asset, archive_path)
}

#[cfg(target_os = "windows")]
fn unpack_release_asset(
    app: &AppHandle,
    game: GameId,
    release: &Release,
    asset: &ReleaseAsset,
    archive_path: &Path,
) -> AppResult<InstallRecord> {
    unpack_windows_zip(app, game, release, asset, archive_path)
}

#[tauri::command]
pub async fn install_release(
    app: AppHandle,
    state: State<'_, AppState>,
    game: GameId,
    release_id: u64,
) -> AppResult<InstallRecord> {
    let mut cached_records = read_installations(&app)?;
    if let Some(cached) = cached_records
        .iter()
        .find(|record| record.game == game && record.release_id == release_id)
        .cloned()
    {
        if Path::new(&cached.executable_path).is_file() {
            let record = activate_cached_installation(
                &mut cached_records,
                game,
                release_id,
                Utc::now().to_rfc3339(),
            )
            .ok_or("캐시된 버전을 활성화하지 못했습니다.")?;
            save_installations(&app, &cached_records)?;
            emit_progress(
                &app,
                release_id,
                "complete",
                0,
                0,
                "캐시된 버전으로 전환했습니다",
            );
            return Ok(record);
        }

        cached_records.retain(|record| !(record.game == game && record.release_id == release_id));
        save_installations(&app, &cached_records)?;
        let stale_dir = PathBuf::from(&cached.install_dir);
        let expected_dir = app_root(&app)?
            .join("installs")
            .join(game.key())
            .join(release_id.to_string());
        if stale_dir == expected_dir && stale_dir.exists() {
            fs::remove_dir_all(&stale_dir)
                .map_err(|error| format!("손상된 설치 캐시를 정리하지 못했습니다: {error}"))?;
        }
    }
    let release = fetch_release(&state, game, release_id).await?;
    let asset = release
        .recommended_asset
        .clone()
        .ok_or("현재 운영체제와 CPU에서 설치할 수 있는 그래픽·사운드 빌드가 없습니다.")?;
    let download = app_root(&app)?.join("downloads").join(format!(
        "{}-{}.download.partial",
        game.key(),
        release.id
    ));
    let result: AppResult<InstallRecord> = async {
        download_asset(&app, &state, release.id, &asset, &download).await?;
        emit_progress(
            &app,
            release.id,
            "unpacking",
            asset.size,
            asset.size,
            "설치 파일을 확인하는 중",
        );
        let app_for_task = app.clone();
        let release_for_task = release.clone();
        let asset_for_task = asset.clone();
        let archive_for_task = download.clone();
        let record = tokio::task::spawn_blocking(move || {
            unpack_release_asset(
                &app_for_task,
                game,
                &release_for_task,
                &asset_for_task,
                &archive_for_task,
            )
        })
        .await
        .map_err(|error| format!("설치 작업을 시작하지 못했습니다: {error}"))??;
        let mut records = read_installations(&app)?;
        records.push(record.clone());
        save_installations(&app, &records)?;
        emit_progress(
            &app,
            release.id,
            "complete",
            asset.size,
            asset.size,
            "설치 완료",
        );
        Ok(record)
    }
    .await;
    let _ = tokio::fs::remove_file(&download).await;
    if let Err(error) = &result {
        emit_progress(&app, release.id, "failed", 0, asset.size, error.clone());
    }
    result
}

const BACKUP_SCHEMA_VERSION: u8 = 1;

fn write_backup_directory(
    writer: &mut ZipWriter<fs::File>,
    source: &Path,
    archive_prefix: &str,
) -> AppResult<()> {
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    writer
        .add_directory(format!("{archive_prefix}/"), options)
        .map_err(|error| format!("백업 폴더를 기록하지 못했습니다: {error}"))?;

    for entry in
        fs::read_dir(source).map_err(|error| format!("백업 폴더를 읽지 못했습니다: {error}"))?
    {
        let entry = entry.map_err(|error| format!("백업 파일을 읽지 못했습니다: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("백업 파일 정보를 읽지 못했습니다: {error}"))?;
        if file_type.is_symlink() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "백업할 파일 이름을 해석하지 못했습니다.")?;
        let archive_path = format!("{archive_prefix}/{name}");
        let path = entry.path();
        if file_type.is_dir() {
            write_backup_directory(writer, &path, &archive_path)?;
        } else if file_type.is_file() {
            writer
                .start_file(&archive_path, options)
                .map_err(|error| format!("백업 파일을 추가하지 못했습니다: {error}"))?;
            let mut input = fs::File::open(&path)
                .map_err(|error| format!("백업 파일을 열지 못했습니다: {error}"))?;
            std::io::copy(&mut input, writer)
                .map_err(|error| format!("백업 파일을 압축하지 못했습니다: {error}"))?;
        }
    }
    Ok(())
}

fn write_backup_archive(
    destination: &Path,
    manifest: &BackupManifest,
    user_dir: &Path,
) -> AppResult<()> {
    let file = fs::File::create(destination)
        .map_err(|error| format!("백업 ZIP 파일을 만들지 못했습니다: {error}"))?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let contents = manifest.contents.clone();
    let manifest = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("백업 정보를 만들지 못했습니다: {error}"))?;
    writer
        .start_file("manifest.json", options)
        .map_err(|error| format!("백업 정보를 ZIP에 추가하지 못했습니다: {error}"))?;
    writer
        .write_all(&manifest)
        .map_err(|error| format!("백업 정보를 ZIP에 기록하지 못했습니다: {error}"))?;

    for directory in &contents {
        let source = user_dir.join(directory);
        if source.is_dir() {
            write_backup_directory(&mut writer, &source, directory)?;
        }
    }
    writer
        .finish()
        .map_err(|error| format!("백업 ZIP 파일을 마무리하지 못했습니다: {error}"))?;
    Ok(())
}

fn backup_contents(user_dir: &Path) -> Vec<String> {
    ["config", "save"]
        .into_iter()
        .filter(|directory| user_dir.join(directory).is_dir())
        .map(str::to_string)
        .collect()
}

fn safe_archive_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn create_backup_snapshot(app: &AppHandle, installation_id: &str) -> AppResult<BackupRecord> {
    let installation = installation_record(app, installation_id)?;
    let user_dir = game_user_dir(app, installation.game)?;
    let contents = backup_contents(&user_dir);
    if contents.is_empty() {
        return Err(
            "백업할 설정 또는 세이브 데이터가 없습니다. 게임을 한 번 실행한 뒤 다시 시도해 주세요."
                .into(),
        );
    }

    let id = Uuid::new_v4().to_string();
    let created_at = Utc::now();
    let backup_dir = app_root(app)?.join("backups").join(&installation.id);
    fs::create_dir_all(&backup_dir)
        .map_err(|error| format!("백업 폴더를 만들지 못했습니다: {error}"))?;
    let filename = format!(
        "cataclysm-{}-{}-{}.zip",
        installation.game.key(),
        safe_archive_segment(&installation.tag),
        created_at.format("%Y%m%d-%H%M%S")
    );
    let archive_path = backup_dir.join(filename);
    let pending_path = backup_dir.join(format!(".{id}.pending"));
    let manifest = BackupManifest {
        schema_version: BACKUP_SCHEMA_VERSION,
        installation_id: installation.id.clone(),
        game: installation.game,
        release_id: installation.release_id,
        tag: installation.tag.clone(),
        created_at: created_at.to_rfc3339(),
        contents: contents.clone(),
    };

    let write_result = write_backup_archive(&pending_path, &manifest, &user_dir);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&pending_path);
        return Err(error);
    }
    fs::rename(&pending_path, &archive_path)
        .map_err(|error| format!("백업 ZIP 파일을 확정하지 못했습니다: {error}"))?;
    let backup = BackupRecord {
        id,
        installation_id: installation.id,
        game: installation.game,
        release_id: installation.release_id,
        tag: installation.tag,
        created_at: created_at.to_rfc3339(),
        archive_path: archive_path.to_string_lossy().to_string(),
        size: fs::metadata(&archive_path)
            .map_err(|error| format!("백업 파일 정보를 읽지 못했습니다: {error}"))?
            .len(),
        contents,
        schema_version: BACKUP_SCHEMA_VERSION,
    };
    let mut backups = read_backups(app)?;
    backups.push(backup.clone());
    save_backups(app, &backups)?;
    Ok(backup)
}

fn is_allowed_backup_path(path: &Path) -> bool {
    let mut components = path.components();
    matches!(
        components.next(),
        Some(std::path::Component::Normal(first)) if first == "config" || first == "save"
    )
}

fn validate_backup_archive(
    archive_path: &Path,
    expected_game: GameId,
) -> AppResult<BackupManifest> {
    let file = fs::File::open(archive_path)
        .map_err(|error| format!("다운로드한 백업을 열지 못했습니다: {error}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("다운로드한 파일은 올바른 ZIP 백업이 아닙니다: {error}"))?;
    let mut manifest_json = String::new();
    archive
        .by_name("manifest.json")
        .map_err(|_| "백업에 manifest.json 파일이 없습니다.")?
        .read_to_string(&mut manifest_json)
        .map_err(|error| format!("백업 정보를 읽지 못했습니다: {error}"))?;
    let manifest: BackupManifest =
        serde_json::from_str(&manifest_json).map_err(|_| "백업 정보 형식이 올바르지 않습니다.")?;
    if manifest.schema_version != BACKUP_SCHEMA_VERSION || manifest.game != expected_game {
        return Err("선택한 게임과 호환되지 않는 백업입니다.".into());
    }
    if manifest.contents.is_empty()
        || manifest
            .contents
            .iter()
            .any(|item| item != "config" && item != "save")
    {
        return Err("백업에 지원하지 않는 데이터 항목이 포함되어 있습니다.".into());
    }
    let mut has_config_directory = false;
    let mut has_save_directory = false;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("백업 파일을 확인하지 못했습니다: {error}"))?;
        let name = entry
            .name()
            .map_err(|error| format!("백업 파일 이름을 읽지 못했습니다: {error}"))?;
        if name == "manifest.json" {
            continue;
        }
        let Some(path) = entry.enclosed_name() else {
            return Err("백업에 안전하지 않은 파일 경로가 포함되어 있습니다.".into());
        };
        if !is_allowed_backup_path(&path) {
            return Err("백업에는 config와 save 파일만 포함할 수 있습니다.".into());
        }
        if entry.is_dir() && path == Path::new("config") {
            has_config_directory = true;
        }
        if entry.is_dir() && path == Path::new("save") {
            has_save_directory = true;
        }
    }
    if manifest.contents.iter().any(|item| item == "config") && !has_config_directory {
        return Err("백업에 config 폴더가 올바르게 포함되어 있지 않습니다.".into());
    }
    if manifest.contents.iter().any(|item| item == "save") && !has_save_directory {
        return Err("백업에 save 폴더가 올바르게 포함되어 있지 않습니다.".into());
    }
    Ok(manifest)
}

fn extract_backup_to_staging(archive_path: &Path, staging: &Path) -> AppResult<()> {
    let file = fs::File::open(archive_path)
        .map_err(|error| format!("다운로드한 백업을 열지 못했습니다: {error}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("다운로드한 파일은 올바른 ZIP 백업이 아닙니다: {error}"))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("백업 파일을 읽지 못했습니다: {error}"))?;
        let name = entry
            .name()
            .map_err(|error| format!("백업 파일 이름을 읽지 못했습니다: {error}"))?;
        if name == "manifest.json" {
            continue;
        }
        let path = entry
            .enclosed_name()
            .ok_or("백업에 안전하지 않은 파일 경로가 포함되어 있습니다.")?;
        if !is_allowed_backup_path(&path) {
            return Err("백업에는 config와 save 파일만 포함할 수 있습니다.".into());
        }
        let destination = staging.join(path);
        if entry.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|error| format!("복원 폴더를 만들지 못했습니다: {error}"))?;
            continue;
        }
        let parent = destination
            .parent()
            .ok_or("복원 경로가 올바르지 않습니다.")?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("복원 폴더를 만들지 못했습니다: {error}"))?;
        let mut output = fs::File::create(&destination)
            .map_err(|error| format!("복원 파일을 만들지 못했습니다: {error}"))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|error| format!("복원 파일을 기록하지 못했습니다: {error}"))?;
    }
    Ok(())
}

fn remove_existing_path(path: &Path) -> AppResult<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path)
            .map_err(|error| format!("기존 게임 데이터를 정리하지 못했습니다: {error}"))
    } else {
        fs::remove_file(path)
            .map_err(|error| format!("기존 게임 데이터를 정리하지 못했습니다: {error}"))
    }
}

fn replace_backup_contents(user_dir: &Path, staging: &Path, work_dir: &Path) -> AppResult<()> {
    fs::create_dir_all(user_dir)
        .map_err(|error| format!("게임 데이터 폴더를 만들지 못했습니다: {error}"))?;
    let rollback = work_dir.join("rollback");
    fs::create_dir_all(&rollback)
        .map_err(|error| format!("복원 준비 폴더를 만들지 못했습니다: {error}"))?;
    let directories = ["config", "save"];
    let replacement = (|| -> AppResult<()> {
        for directory in directories {
            let current = user_dir.join(directory);
            if current.exists() {
                fs::rename(&current, rollback.join(directory))
                    .map_err(|error| format!("현재 게임 데이터를 보호하지 못했습니다: {error}"))?;
            }
        }
        for directory in directories {
            let staged = staging.join(directory);
            if staged.exists() {
                fs::rename(&staged, user_dir.join(directory))
                    .map_err(|error| format!("다운로드한 백업을 적용하지 못했습니다: {error}"))?;
            }
        }
        Ok(())
    })();
    if replacement.is_ok() {
        return Ok(());
    }

    for directory in directories {
        let current = user_dir.join(directory);
        let previous = rollback.join(directory);
        let _ = remove_existing_path(&current);
        if previous.exists() {
            let _ = fs::rename(previous, current);
        }
    }
    replacement
}

fn restore_downloaded_backup(
    app: &AppHandle,
    installation: &InstallRecord,
    archive_path: &Path,
) -> AppResult<BackupRecord> {
    validate_backup_archive(archive_path, installation.game)?;
    let work_dir = app_root(app)?
        .join("restore")
        .join(Uuid::new_v4().to_string());
    let staging = work_dir.join("staging");
    fs::create_dir_all(&staging)
        .map_err(|error| format!("복원 준비 폴더를 만들지 못했습니다: {error}"))?;
    let result = (|| -> AppResult<BackupRecord> {
        extract_backup_to_staging(archive_path, &staging)?;
        let safety_backup = create_backup_snapshot(app, &installation.id)?;
        replace_backup_contents(&game_user_dir(app, installation.game)?, &staging, &work_dir)?;
        Ok(safety_backup)
    })();
    let _ = fs::remove_dir_all(&work_dir);
    result
}

fn open_in_file_manager(path: &Path, reveal: bool, description: &str) -> AppResult<()> {
    if !path.exists() {
        return Err(format!(
            "{description}이 아직 없습니다. 게임을 한 번 실행한 뒤 다시 시도해 주세요."
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("/usr/bin/open");
        if reveal {
            command.arg("-R");
        }
        command
            .arg(path)
            .spawn()
            .map_err(|error| format!("Finder에서 {description}을 열지 못했습니다: {error}"))?;
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer.exe");
        if reveal {
            command.arg("/select,");
        }
        command
            .arg(path)
            .spawn()
            .map_err(|error| format!("파일 탐색기에서 {description}을 열지 못했습니다: {error}"))?;
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = reveal;
        return Err("이 운영체제에서는 파일 위치 열기를 지원하지 않습니다.".into());
    }
    Ok(())
}

#[tauri::command]
pub fn reveal_installation_location(
    app: AppHandle,
    installation_id: String,
    location: InstallationLocation,
) -> AppResult<()> {
    let installation = installation_record(&app, &installation_id)?;
    let (path, description) = match location {
        InstallationLocation::Install => (PathBuf::from(installation.install_dir), "설치 폴더"),
        InstallationLocation::Config => (
            game_user_dir(&app, installation.game)?.join("config"),
            "설정 폴더",
        ),
        InstallationLocation::Save => (
            game_user_dir(&app, installation.game)?.join("save"),
            "세이브 폴더",
        ),
    };
    open_in_file_manager(&path, false, description)
}

#[tauri::command]
pub async fn create_backup(app: AppHandle, installation_id: String) -> AppResult<BackupRecord> {
    let app_for_task = app.clone();
    tokio::task::spawn_blocking(move || create_backup_snapshot(&app_for_task, &installation_id))
        .await
        .map_err(|error| format!("백업 작업을 시작하지 못했습니다: {error}"))?
}

#[tauri::command]
pub fn list_backups(app: AppHandle, installation_id: String) -> AppResult<Vec<BackupRecord>> {
    let mut backups = read_backups(&app)?
        .into_iter()
        .filter(|backup| backup.installation_id == installation_id)
        .collect::<Vec<_>>();
    backups.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(backups)
}

#[tauri::command]
pub fn reveal_backup(app: AppHandle, backup_id: String) -> AppResult<()> {
    let backup = backup_record(&app, &backup_id)?;
    open_in_file_manager(Path::new(&backup.archive_path), true, "백업 파일")
}

#[tauri::command]
pub fn export_backup(app: AppHandle, backup_id: String) -> AppResult<bool> {
    let backup = backup_record(&app, &backup_id)?;
    let source = PathBuf::from(&backup.archive_path);
    if !source.is_file() {
        return Err("내보낼 백업 파일이 없습니다. 백업을 새로 만든 뒤 다시 시도해 주세요.".into());
    }
    let default_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cataclysm-backup.zip");
    let Some(mut destination) = rfd::FileDialog::new()
        .add_filter("Cataclysm Backup", &["zip"])
        .set_file_name(default_name)
        .save_file()
    else {
        return Ok(false);
    };
    if destination.extension().is_none() {
        destination.set_extension("zip");
    }
    if destination == source {
        return Err("백업 원본과 같은 위치로는 내보낼 수 없습니다.".into());
    }
    fs::copy(&source, &destination)
        .map_err(|error| format!("백업 파일을 내보내지 못했습니다: {error}"))?;
    Ok(true)
}

#[tauri::command]
pub fn get_webdav_connection(app: AppHandle) -> AppResult<Option<WebDavConnection>> {
    read_webdav_connection(&app)
}

#[tauri::command]
pub async fn test_webdav_connection(
    state: State<'_, AppState>,
    input: WebDavConnectionInput,
) -> AppResult<()> {
    let (connection, password) = normalize_webdav_connection(input)?;
    check_webdav_connection(&state, &connection, &password).await
}

#[tauri::command]
pub async fn save_webdav_connection(
    app: AppHandle,
    state: State<'_, AppState>,
    input: WebDavConnectionInput,
) -> AppResult<WebDavConnection> {
    let (connection, password) = normalize_webdav_connection(input)?;
    check_webdav_connection(&state, &connection, &password).await?;
    store_webdav_password(&state, &password)?;
    ensure_webdav_directories(&state, &connection, &password).await?;
    persist_webdav_connection(&app, &connection)?;
    Ok(connection)
}

#[tauri::command]
pub fn disconnect_webdav(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    let config = webdav_config_file(&app)?;
    if config.exists() {
        fs::remove_file(config)
            .map_err(|error| format!("WebDAV 설정을 제거하지 못했습니다: {error}"))?;
    }
    clear_webdav_password(&state)?;
    let _ = webdav_keychain_entry()?.delete_credential();
    Ok(())
}

#[tauri::command]
pub async fn list_remote_backups(
    app: AppHandle,
    state: State<'_, AppState>,
    game: GameId,
) -> AppResult<Vec<RemoteBackupRecord>> {
    let connection = read_webdav_connection(&app)?.ok_or("WebDAV 서버를 먼저 연결해 주세요.")?;
    let password = read_webdav_password(&state)?;
    let listing = webdav_listing(
        &state,
        webdav_game_url(&connection, game)?,
        &connection.username,
        &password,
        "1",
    )
    .await?;
    let mut backups = listing
        .into_iter()
        .filter(|item| !item.is_collection)
        .filter_map(|item| {
            remote_file_name_from_href(&item.href).map(|file_name| RemoteBackupRecord {
                file_name: file_name.to_string(),
                game,
                size: item.size.unwrap_or_default(),
                modified_at: item.modified_at,
            })
        })
        .collect::<Vec<_>>();
    backups.sort_by(|left, right| right.modified_at.cmp(&left.modified_at));
    Ok(backups)
}

#[tauri::command]
pub async fn upload_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    backup_id: String,
) -> AppResult<RemoteBackupRecord> {
    let backup = backup_record(&app, &backup_id)?;
    let connection = read_webdav_connection(&app)?.ok_or("WebDAV 서버를 먼저 연결해 주세요.")?;
    let password = read_webdav_password(&state)?;
    let source = PathBuf::from(&backup.archive_path);
    if !source.is_file() {
        return Err("업로드할 로컬 백업 파일을 찾지 못했습니다.".into());
    }
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or("백업 파일 이름을 읽지 못했습니다.")?;
    let file_name = format!("{stem}-{}.zip", backup.id);
    let destination = webdav_backup_url(&connection, backup.game, &file_name)?;
    let file = tokio::fs::File::open(&source)
        .await
        .map_err(|error| format!("업로드할 백업 파일을 열지 못했습니다: {error}"))?;
    let body = reqwest::Body::wrap_stream(ReaderStream::new(file));
    let response = state
        .webdav_http
        .put(destination)
        .header(header::CONTENT_TYPE, "application/zip")
        .basic_auth(&connection.username, Some(&password))
        .body(body)
        .send()
        .await
        .map_err(|error| format!("WebDAV에 백업을 업로드하지 못했습니다: {error}"))?;
    if !response.status().is_success() {
        return Err(webdav_error(response.status()));
    }
    Ok(RemoteBackupRecord {
        file_name,
        game: backup.game,
        size: backup.size,
        modified_at: Some(Utc::now().to_rfc3339()),
    })
}

#[tauri::command]
pub async fn restore_remote_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    installation_id: String,
    file_name: String,
) -> AppResult<BackupRecord> {
    let installation = installation_record(&app, &installation_id)?;
    let connection = read_webdav_connection(&app)?.ok_or("WebDAV 서버를 먼저 연결해 주세요.")?;
    let password = read_webdav_password(&state)?;
    let response = state
        .webdav_http
        .get(webdav_backup_url(
            &connection,
            installation.game,
            &file_name,
        )?)
        .basic_auth(&connection.username, Some(&password))
        .send()
        .await
        .map_err(|error| format!("WebDAV에서 백업을 다운로드하지 못했습니다: {error}"))?;
    if !response.status().is_success() {
        return Err(webdav_error(response.status()));
    }
    let download_dir = app_root(&app)?.join("cloud-downloads");
    tokio::fs::create_dir_all(&download_dir)
        .await
        .map_err(|error| format!("다운로드 폴더를 만들지 못했습니다: {error}"))?;
    let download_path = download_dir.join(format!("{}.zip", Uuid::new_v4()));
    let download_result: AppResult<()> = async {
        let mut output = tokio::fs::File::create(&download_path)
            .await
            .map_err(|error| format!("다운로드 파일을 만들지 못했습니다: {error}"))?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| format!("WebDAV 백업을 받지 못했습니다: {error}"))?;
            output
                .write_all(&chunk)
                .await
                .map_err(|error| format!("다운로드 파일을 기록하지 못했습니다: {error}"))?;
        }
        output
            .flush()
            .await
            .map_err(|error| format!("다운로드 파일을 마무리하지 못했습니다: {error}"))?;
        Ok(())
    }
    .await;
    if let Err(error) = download_result {
        let _ = tokio::fs::remove_file(&download_path).await;
        return Err(error);
    }
    let app_for_task = app.clone();
    let installation_for_task = installation.clone();
    let archive_for_task = download_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        restore_downloaded_backup(&app_for_task, &installation_for_task, &archive_for_task)
    })
    .await
    .map_err(|error| format!("백업 복원 작업을 시작하지 못했습니다: {error}"))?;
    let _ = tokio::fs::remove_file(download_path).await;
    result
}

#[tauri::command]
pub fn list_installations(app: AppHandle) -> AppResult<Vec<InstallRecord>> {
    Ok(active_installations(&read_installations(&app)?))
}

#[tauri::command]
pub fn launch_installation(app: AppHandle, installation_id: String) -> AppResult<()> {
    let record = read_installations(&app)?
        .into_iter()
        .find(|record| record.id == installation_id)
        .ok_or("설치된 버전을 찾지 못했습니다.")?;
    let executable = PathBuf::from(&record.executable_path);
    if !executable.is_file() {
        return Err("게임 실행 파일이 없습니다. 설치 폴더를 확인해 주세요.".into());
    }
    let resources = executable
        .parent()
        .ok_or("게임 실행 경로가 올바르지 않습니다")?;
    let user_dir = game_user_dir(&app, record.game)?;
    fs::create_dir_all(&user_dir)
        .map_err(|error| format!("게임 데이터 폴더를 만들지 못했습니다: {error}"))?;
    let mut command = Command::new(&executable);
    command
        .current_dir(resources)
        .arg("--userdir")
        .arg(&user_dir);
    #[cfg(target_os = "macos")]
    command
        .env("DYLD_LIBRARY_PATH", ".")
        .env("DYLD_FRAMEWORK_PATH", ".");
    command
        .spawn()
        .map_err(|error| format!("게임을 실행하지 못했습니다: {error}"))?;
    Ok(())
}

#[tauri::command]
pub fn reveal_installation(app: AppHandle, installation_id: String) -> AppResult<()> {
    reveal_installation_location(app, installation_id, InstallationLocation::Install)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, net::TcpListener, thread};

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            id: 1,
            name: name.to_string(),
            size: 42,
            url: "https://example.com/game.dmg".to_string(),
        }
    }

    fn installation(game: GameId, release_id: u64, installed_at: &str) -> InstallRecord {
        InstallRecord {
            id: format!("{}-{release_id}", game.key()),
            game,
            release_id,
            tag: format!("version-{release_id}"),
            name: format!("Version {release_id}"),
            asset_name: format!("build-{release_id}.dmg"),
            installed_at: installed_at.to_string(),
            install_dir: format!("/cache/{}/{release_id}", game.key()),
            user_dir: format!("/userdata/{}", game.key()),
            executable_path: format!("/cache/{}/{release_id}/game", game.key()),
        }
    }

    #[test]
    fn one_active_installation_per_game_reuses_cached_records() {
        let mut records = vec![
            installation(GameId::Dda, 1, "2026-08-18T00:00:00Z"),
            installation(GameId::Dda, 2, "2026-08-19T00:00:00Z"),
            installation(GameId::Bn, 3, "2026-08-17T00:00:00Z"),
        ];

        let active = active_installations(&records);
        assert_eq!(active.len(), 2);
        assert!(active
            .iter()
            .any(|record| record.game == GameId::Dda && record.release_id == 2));
        assert!(active
            .iter()
            .any(|record| record.game == GameId::Bn && record.release_id == 3));

        let reactivated = activate_cached_installation(
            &mut records,
            GameId::Dda,
            1,
            "2026-08-20T00:00:00Z".to_string(),
        )
        .expect("cached installation should activate");
        assert_eq!(reactivated.release_id, 1);
        assert_eq!(records.len(), 3, "inactive versions remain cached");

        let active = active_installations(&records);
        assert!(active
            .iter()
            .any(|record| record.game == GameId::Dda && record.release_id == 1));
        assert!(!active
            .iter()
            .any(|record| record.game == GameId::Dda && record.release_id == 2));
    }

    #[test]
    fn json_persistence_replaces_an_existing_file() {
        let root = std::env::temp_dir().join(format!("cataclysm-hub-json-test-{}", Uuid::new_v4()));
        let path = root.join("state.json");
        write_json(&path, &serde_json::json!({ "revision": 1 })).expect("first write");
        write_json(&path, &serde_json::json!({ "revision": 2 })).expect("replacement write");

        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read replacement"))
                .expect("valid replacement JSON");
        assert_eq!(value["revision"], 2);
        fs::remove_dir_all(root).expect("test cleanup");
    }

    #[test]
    fn dda_prefers_graphics_universal_build_over_terminal_build() {
        let selected = recommend_asset_for(
            GameId::Dda,
            &[
                asset("cdda-osx-terminal-only-universal.dmg"),
                asset("cdda-osx-with-graphics-universal.dmg"),
                asset("cdda-osx-with-graphics-universal-no-soundpack.dmg"),
            ],
            "macos",
            "aarch64",
        )
        .expect("a graphics build should be selected");

        assert_eq!(selected.name, "cdda-osx-with-graphics-universal.dmg");
    }

    #[test]
    fn bn_selects_the_current_architecture_tiles_build() {
        let arm_selected = recommend_asset_for(
            GameId::Bn,
            &[
                asset("cbn-osx-tiles-arm-build.dmg"),
                asset("cbn-osx-tiles-x64-build.dmg"),
                asset("cbn-osx-curses-arm-build.dmg"),
            ],
            "macos",
            "aarch64",
        )
        .expect("a matching architecture build should be selected");
        assert_eq!(arm_selected.name, "cbn-osx-tiles-arm-build.dmg");

        let x64_selected = recommend_asset_for(
            GameId::Bn,
            &[
                asset("cbn-osx-tiles-arm-build.dmg"),
                asset("cbn-osx-tiles-x64-build.dmg"),
            ],
            "macos",
            "x86_64",
        )
        .expect("an x64 build should be selected");
        assert_eq!(x64_selected.name, "cbn-osx-tiles-x64-build.dmg");
    }

    #[test]
    fn windows_selects_graphics_sound_builds_and_ignores_symbols() {
        let dda = recommend_asset_for(
            GameId::Dda,
            &[
                asset("cdda-windows-with-graphics-x64-build.zip"),
                asset("cdda-windows-with-graphics-and-sounds-x64-build.zip"),
                asset("cdda-osx-with-graphics-universal-build.dmg"),
            ],
            "windows",
            "x86_64",
        )
        .expect("DDA Windows build");
        assert_eq!(
            dda.name,
            "cdda-windows-with-graphics-and-sounds-x64-build.zip"
        );

        let bn = recommend_asset_for(
            GameId::Bn,
            &[
                asset("cbn-windows-tiles-x64-msvc-build-pdb.zip"),
                asset("cbn-windows-tiles-x64-msvc-no-soundpack-build.zip"),
                asset("cbn-windows-tiles-x64-msvc-build.zip"),
            ],
            "windows",
            "x86_64",
        )
        .expect("BN Windows build");
        assert_eq!(bn.name, "cbn-windows-tiles-x64-msvc-build.zip");

        assert!(recommend_asset_for(GameId::Dda, &[dda], "windows", "aarch64").is_none());
    }

    #[test]
    fn windows_zip_is_extracted_and_graphical_executable_is_found() {
        let root =
            std::env::temp_dir().join(format!("cataclysm-hub-install-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("test root");
        let archive_path = root.join("game.zip");
        let destination = root.join("game");
        let file = fs::File::create(&archive_path).expect("zip file");
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer
            .start_file("distribution/Cataclysm.exe", options)
            .expect("executable entry");
        writer.write_all(b"MZtest").expect("executable contents");
        writer
            .start_file("distribution/data/core.json", options)
            .expect("data entry");
        writer.write_all(b"{}").expect("data contents");
        writer.finish().expect("finish zip");

        extract_windows_game_zip(&archive_path, &destination).expect("extract game");
        let executable =
            find_windows_executable(&destination, GameId::Dda).expect("find graphical executable");
        assert_eq!(
            executable.file_name().and_then(|name| name.to_str()),
            Some("Cataclysm.exe")
        );
        assert!(destination
            .join("distribution")
            .join("data")
            .join("core.json")
            .is_file());
        assert!(!is_safe_windows_archive_path(Path::new("../escape.exe")));
        assert!(!is_safe_windows_archive_path(Path::new("CON")));
        fs::remove_dir_all(root).expect("test cleanup");
    }

    #[test]
    fn backup_archive_contains_only_manifest_config_and_save() {
        let root =
            std::env::temp_dir().join(format!("cataclysm-hub-backup-test-{}", Uuid::new_v4()));
        let user_dir = root.join("user");
        fs::create_dir_all(user_dir.join("config")).expect("config dir");
        fs::create_dir_all(user_dir.join("save").join("world-1")).expect("save dir");
        fs::create_dir_all(user_dir.join("memorial")).expect("unselected dir");
        fs::write(user_dir.join("config").join("options.json"), b"{}").expect("config file");
        fs::write(
            user_dir.join("save").join("world-1").join("master.gsav"),
            b"save",
        )
        .expect("save file");
        fs::write(user_dir.join("memorial").join("old.txt"), b"not included")
            .expect("unselected file");

        let archive = root.join("snapshot.zip");
        let manifest = BackupManifest {
            schema_version: BACKUP_SCHEMA_VERSION,
            installation_id: "dda-1".to_string(),
            game: GameId::Dda,
            release_id: 1,
            tag: "test".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            contents: vec!["config".to_string(), "save".to_string()],
        };
        write_backup_archive(&archive, &manifest, &user_dir).expect("backup archive");

        let file = fs::File::open(&archive).expect("archive file");
        let mut zip = zip::ZipArchive::new(file).expect("valid zip");
        let mut manifest_file = zip.by_name("manifest.json").expect("manifest entry");
        let mut manifest_json = String::new();
        manifest_file
            .read_to_string(&mut manifest_json)
            .expect("manifest text");
        drop(manifest_file);
        assert!(manifest_json.contains("dda-1"));
        assert!(zip.by_name("config/options.json").is_ok());
        assert!(zip.by_name("save/world-1/master.gsav").is_ok());
        assert!(zip.by_name("memorial/old.txt").is_err());

        drop(zip);
        assert_eq!(
            validate_backup_archive(&archive, GameId::Dda)
                .expect("validated backup")
                .tag,
            "test"
        );
        fs::remove_dir_all(root).expect("test cleanup");
    }

    #[test]
    fn webdav_listing_keeps_only_safe_zip_files() {
        let listing = parse_webdav_listing(
            r#"<?xml version="1.0"?>
            <d:multistatus xmlns:d="DAV:">
              <d:response><d:href>/dav/CataclysmHub/dda/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
              <d:response><d:href>/dav/CataclysmHub/dda/cataclysm-dda-0.H-abc.zip</d:href><d:propstat><d:prop><d:getcontentlength>42</d:getcontentlength><d:getlastmodified>Tue, 01 Jan 2026 00:00:00 GMT</d:getlastmodified></d:prop></d:propstat></d:response>
              <d:response><d:href>/dav/CataclysmHub/dda/../unsafe.zip</d:href><d:propstat><d:prop><d:getcontentlength>9</d:getcontentlength></d:prop></d:propstat></d:response>
            </d:multistatus>"#,
        )
        .expect("listing should parse");
        assert!(listing.iter().any(|item| item.is_collection));
        let files = listing
            .into_iter()
            .filter(|item| !item.is_collection)
            .filter_map(|item| remote_file_name_from_href(&item.href).map(str::to_string))
            .collect::<Vec<_>>();

        assert_eq!(files, vec!["cataclysm-dda-0.H-abc.zip"]);
    }

    #[test]
    fn webdav_connection_allows_http_and_rejects_unsafe_configuration() {
        let http = normalize_webdav_connection(WebDavConnectionInput {
            endpoint: "http://nas.local/dav".to_string(),
            username: "user".to_string(),
            password: "secret".to_string(),
            root_folder: "CataclysmHub".to_string(),
        });
        assert!(http.is_ok());

        let unsupported_protocol = normalize_webdav_connection(WebDavConnectionInput {
            endpoint: "ftp://nas.local/dav".to_string(),
            username: "user".to_string(),
            password: "secret".to_string(),
            root_folder: "CataclysmHub".to_string(),
        });
        assert!(unsupported_protocol.is_err());

        let unsafe_folder = normalize_webdav_connection(WebDavConnectionInput {
            endpoint: "https://cloud.example.com/dav".to_string(),
            username: "user".to_string(),
            password: "secret".to_string(),
            root_folder: "CataclysmHub/../other".to_string(),
        });
        assert!(unsafe_folder.is_err());
    }

    #[test]
    fn webdav_collection_url_fallback_toggles_trailing_slash() {
        let with_slash = Url::parse("https://cloud.example.com/dav/folder/").expect("url");
        let without_slash = alternate_collection_url(with_slash).expect("alternate without slash");
        assert_eq!(without_slash.path(), "/dav/folder");

        let restored = alternate_collection_url(without_slash).expect("alternate with slash");
        assert_eq!(restored.path(), "/dav/folder/");
        assert!(
            alternate_collection_url(Url::parse("https://cloud.example.com/").expect("root"))
                .is_none()
        );
    }

    #[test]
    fn webdav_propfind_requests_required_listing_properties() {
        assert!(WEBDAV_PROPFIND_BODY.contains("<d:resourcetype/>"));
        assert!(WEBDAV_PROPFIND_BODY.contains("<d:getcontentlength/>"));
        assert!(WEBDAV_PROPFIND_BODY.contains("<d:getlastmodified/>"));
    }

    #[test]
    fn webdav_propfind_bypasses_proxy_and_retries_405_without_trailing_slash() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server");
        let address = listener.local_addr().expect("test server address");
        let server = thread::spawn(move || {
            for (request_number, expected_path) in [(1, "/dav/folder/"), (2, "/dav/folder")] {
                let (mut stream, _) = listener.accept().expect("test request");
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0_u8; 4096];
                    let count = stream.read(&mut chunk).expect("read test request");
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    let request = String::from_utf8_lossy(&bytes);
                    let Some(header_end) = request.find("\r\n\r\n") else {
                        continue;
                    };
                    let content_length = request[..header_end]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or_default();
                    if bytes.len() >= header_end + 4 + content_length {
                        break;
                    }
                }

                let request = String::from_utf8(bytes).expect("UTF-8 test request");
                assert!(request.starts_with(&format!("PROPFIND {expected_path} HTTP/1.1")));
                assert!(request.to_ascii_lowercase().contains("depth: 1"));
                assert!(request.contains(WEBDAV_PROPFIND_BODY));

                if request_number == 1 {
                    stream
                        .write_all(
                            b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("write 405 response");
                } else {
                    let body = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
  <d:response><d:href>/dav/folder/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
</d:multistatus>"#;
                    let response = format!(
                        "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("write 207 response");
                }
            }
        });

        let state = AppState {
            http: reqwest::Client::builder()
                .proxy(reqwest::Proxy::all("http://127.0.0.1:9").expect("test proxy"))
                .build()
                .expect("proxied HTTP client"),
            webdav_http: reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("direct WebDAV client"),
            webdav_password: Mutex::new(None),
        };
        let url = Url::parse(&format!("http://{address}/dav/folder/")).expect("test URL");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let listing = runtime
            .block_on(webdav_listing(&state, url, "user", "password", "1"))
            .expect("fallback listing");
        server.join().expect("test server completion");
        assert!(listing.iter().any(|item| item.is_collection));
    }

    #[test]
    fn webdav_password_is_cached_for_the_current_app_session() {
        let state = AppState {
            http: reqwest::Client::new(),
            webdav_http: reqwest::Client::new(),
            webdav_password: Mutex::new(Some("cached-password".to_string())),
        };

        assert_eq!(
            read_webdav_password(&state).expect("cached password"),
            "cached-password"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn webdav_password_uses_macos_keychain_backend() {
        let entry = webdav_keychain_entry().expect("keychain entry");

        assert!(entry
            .get_credential()
            .downcast_ref::<keyring::macos::MacCredential>()
            .is_some());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn webdav_password_uses_windows_credential_backend() {
        let entry = webdav_keychain_entry().expect("credential entry");

        assert!(entry
            .get_credential()
            .downcast_ref::<keyring::windows::WinCredential>()
            .is_some());
    }
}
