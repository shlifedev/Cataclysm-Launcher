import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { formatBytes, formatDate, formatDateTime } from "./lib/format";
import type {
  BackupRecord,
  GameId,
  InstallProgress,
  InstallRecord,
  Release,
  ReleasePage,
  RemoteBackupRecord,
  WebDavConnection,
  WebDavConnectionInput,
} from "./types";

type Filter = "all" | "stable" | "experimental";
type Installing = Record<number, InstallProgress | undefined>;
type InstallationLocation = "install" | "config" | "save";
type LoadStatus = "idle" | "loading" | "loaded" | "error";
type VersionChannel = "Stable" | "Experimental" | "Nightly";
type RestoreConfirmation = {
  installation: InstallRecord;
  backup: RemoteBackupRecord;
};

const games: Array<{ id: GameId; label: string; subtitle: string }> = [
  { id: "dda", label: "CDDA", subtitle: "Dark Days Ahead" },
  { id: "bn", label: "CBN", subtitle: "Bright Nights" },
];

function matchesFilter(release: Release, filter: Filter) {
  if (filter === "all") return true;
  return filter === "experimental" ? release.prerelease : !release.prerelease;
}

function formatVersionTag(tag: string) {
  const experimental = tag.match(/^cdda-experimental-(\d{4})-(\d{2})-(\d{2})-(\d{2})(\d{2})$/i);
  if (experimental) {
    const [, year, month, day, hour, minute] = experimental;
    return `${year}.${month}.${day} ${hour}:${minute}`;
  }
  const nightly = tag.match(/^(\d{4})-(\d{2})-(\d{2})$/);
  if (nightly) return nightly.slice(1).join(".");
  return tag.replace(/^(?:cdda|cbn)-/i, "");
}

function versionChannel(tag: string, game: GameId): VersionChannel {
  if (/experimental/i.test(tag)) return "Experimental";
  if (game === "bn" && /^\d{4}-\d{2}-\d{2}$/.test(tag)) return "Nightly";
  return "Stable";
}

function releaseChannel(release: Release, game: GameId): VersionChannel {
  if (!release.prerelease) return "Stable";
  return game === "bn" ? "Nightly" : "Experimental";
}

function formatInstalledVersion(record: InstallRecord) {
  return `${versionChannel(record.tag, record.game)} · ${formatVersionTag(record.tag)}`;
}

function ChevronDownIcon() {
  return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="m5.5 7.5 4.5 4.5 4.5-4.5" /></svg>;
}

function RefreshIcon() {
  return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M15.8 8.2A6.2 6.2 0 1 0 16 11" /><path d="M15.8 4.5v3.8H12" /></svg>;
}

function PlayIcon() {
  return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="m7.5 5.5 6.5 4.5-6.5 4.5z" /></svg>;
}

function FolderIcon() {
  return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M2.8 5.7c0-.8.7-1.5 1.5-1.5h3.3l1.6 1.7h6.5c.8 0 1.5.6 1.5 1.5v7.1c0 .8-.7 1.5-1.5 1.5H4.3c-.8 0-1.5-.7-1.5-1.5z" /></svg>;
}

function ArchiveIcon() {
  return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M3.4 5.2h13.2v10.2H3.4z" /><path d="M2.5 3.4h15v2.2h-15zM8 9.1h4" /></svg>;
}

function DownloadIcon() {
  return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M10 3.5v8.1" /><path d="m6.8 8.6 3.2 3.2 3.2-3.2M4 15.5h12" /></svg>;
}

function CloseIcon() {
  return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="m6 6 8 8M14 6l-8 8" /></svg>;
}

function CheckIcon() {
  return <svg viewBox="0 0 20 20" aria-hidden="true"><path d="m5.5 10.1 2.9 2.9 6.2-6.2" /></svg>;
}

function SettingsIcon() {
  return <svg viewBox="0 0 20 20" aria-hidden="true"><circle cx="10" cy="10" r="2.5" /><path d="m10 3.2.6 1.4 1.5.6 1.4-.6 1.5 1.5-.6 1.4.6 1.5 1.4.6v2l-1.4.6-.6 1.5.6 1.4-1.5 1.5-1.4-.6-1.5.6-.6 1.4h-2l-.6-1.4-1.5-.6-1.4.6-1.5-1.5.6-1.4-.6-1.5-1.4-.6v-2l1.4-.6.6-1.5-.6-1.4 1.5-1.5 1.4.6 1.5-.6z" /></svg>;
}

const emptyWebDavForm: WebDavConnectionInput = {
  endpoint: "",
  username: "",
  password: "",
  rootFolder: "CataclysmHub",
};

export function App() {
  const [activeGame, setActiveGame] = useState<GameId>("dda");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [versionDialogOpen, setVersionDialogOpen] = useState(false);
  const [filter, setFilter] = useState<Filter>("all");
  const [releases, setReleases] = useState<Record<GameId, Release[]>>({ dda: [], bn: [] });
  const [page, setPage] = useState<Record<GameId, number>>({ dda: 0, bn: 0 });
  const [hasMore, setHasMore] = useState<Record<GameId, boolean>>({ dda: true, bn: true });
  const [loading, setLoading] = useState(false);
  const [installed, setInstalled] = useState<InstallRecord[]>([]);
  const [installing, setInstalling] = useState<Installing>({});
  const [expandedReleaseKey, setExpandedReleaseKey] = useState<string>();
  const [backups, setBackups] = useState<Record<string, BackupRecord[]>>({});
  const [localBackupStatus, setLocalBackupStatus] = useState<Record<string, LoadStatus>>({});
  const [backupBusyId, setBackupBusyId] = useState<string>();
  const [webdavConnection, setWebdavConnection] = useState<WebDavConnection>();
  const [webdavForm, setWebdavForm] = useState<WebDavConnectionInput>(emptyWebDavForm);
  const [webdavDialogOpen, setWebdavDialogOpen] = useState(false);
  const [remoteBackups, setRemoteBackups] = useState<Record<GameId, RemoteBackupRecord[]>>({ dda: [], bn: [] });
  const [remoteBackupStatus, setRemoteBackupStatus] = useState<Record<GameId, LoadStatus>>({ dda: "idle", bn: "idle" });
  const [cloudBusy, setCloudBusy] = useState<string>();
  const [restoreConfirmation, setRestoreConfirmation] = useState<RestoreConfirmation>();
  const [backupNotice, setBackupNotice] = useState<string>();
  const [error, setError] = useState<string>();
  const requestedPages = useRef(new Set<string>());
  const requestedLocalBackups = useRef(new Set<string>());
  const requestedRemoteBackups = useRef(new Set<GameId>());

  const refreshInstalled = useCallback(async () => {
    try {
      setInstalled(await invoke<InstallRecord[]>("list_installations"));
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  const loadPage = useCallback(async (game: GameId, nextPage: number, refresh = false) => {
    const requestKey = `${game}:${nextPage}`;
    if (requestedPages.current.has(requestKey) && !refresh) return;
    requestedPages.current.add(requestKey);
    setLoading(true);
    setError(undefined);
    try {
      const result = await invoke<ReleasePage>("fetch_releases", { game, page: nextPage, refresh });
      setReleases((current) => ({
        ...current,
        [game]: nextPage === 1 ? result.releases : [...current[game], ...result.releases],
      }));
      setPage((current) => ({ ...current, [game]: nextPage }));
      setHasMore((current) => ({ ...current, [game]: result.hasMore }));
    } catch (reason) {
      requestedPages.current.delete(requestKey);
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refreshInstalled();
  }, [refreshInstalled]);

  useEffect(() => {
    void invoke<WebDavConnection | null>("get_webdav_connection")
      .then((connection) => setWebdavConnection(connection ?? undefined))
      .catch((reason) => setError(String(reason)));
  }, []);

  useEffect(() => {
    if (releases[activeGame].length === 0) {
      void loadPage(activeGame, 1);
    }
  }, [activeGame, loadPage, releases]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<InstallProgress>("install-progress", (event) => {
      setInstalling((current) => ({ ...current, [event.payload.releaseId]: event.payload }));
    }).then((dispose) => {
      unlisten = dispose;
    });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    function closeOverlay(event: KeyboardEvent) {
      if (event.key !== "Escape") return;
      if (restoreConfirmation) {
        setRestoreConfirmation(undefined);
        return;
      }
      if (webdavDialogOpen) {
        if (!cloudBusy) setWebdavDialogOpen(false);
        return;
      }
      if (versionDialogOpen) {
        setVersionDialogOpen(false);
      }
    }

    window.addEventListener("keydown", closeOverlay);
    return () => window.removeEventListener("keydown", closeOverlay);
  }, [cloudBusy, restoreConfirmation, versionDialogOpen, webdavDialogOpen]);

  const gameReleases = releases[activeGame];
  const activeGameInfo = games.find((game) => game.id === activeGame)!;
  const visibleReleases = useMemo(
    () => gameReleases.filter((release) => matchesFilter(release, filter)),
    [filter, gameReleases],
  );
  const currentInstallation = useMemo(
    () => installed.find((item) => item.game === activeGame),
    [activeGame, installed],
  );
  const installedReleaseIds = useMemo(
    () => new Set(currentInstallation ? [currentInstallation.releaseId] : []),
    [currentInstallation],
  );
  const latestChannelRelease = useMemo(() => {
    if (!currentInstallation) return undefined;
    const channel = versionChannel(currentInstallation.tag, currentInstallation.game);
    return gameReleases.find((release) => release.recommendedAsset && releaseChannel(release, activeGame) === channel);
  }, [activeGame, currentInstallation, gameReleases]);
  const updateRelease = latestChannelRelease?.id !== currentInstallation?.releaseId ? latestChannelRelease : undefined;
  const updateProgress = updateRelease ? installing[updateRelease.id] : undefined;
  const updatePercent = updateProgress
    ? Math.min(100, Math.round((updateProgress.receivedBytes / Math.max(updateProgress.totalBytes, 1)) * 100))
    : 0;
  const currentLocalBackups = currentInstallation ? backups[currentInstallation.id] ?? [] : [];
  const currentLocalBackupStatus = currentInstallation ? localBackupStatus[currentInstallation.id] ?? "idle" : "idle";

  async function install(release: Release) {
    setError(undefined);
    setInstalling((current) => ({
      ...current,
      [release.id]: {
        releaseId: release.id,
        stage: "downloading",
        receivedBytes: 0,
        totalBytes: release.recommendedAsset?.size ?? 0,
        message: "설치를 준비하고 있어요",
      },
    }));
    try {
      const record = await invoke<InstallRecord>("install_release", { game: activeGame, releaseId: release.id });
      setInstalled((current) => [...current.filter((item) => item.game !== record.game), record]);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setInstalling((current) => {
        const next = { ...current };
        delete next[release.id];
        return next;
      });
    }
  }

  async function launch(record: InstallRecord) {
    setError(undefined);
    try {
      await invoke("launch_installation", { installationId: record.id });
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function openLocation(record: InstallRecord, location: InstallationLocation) {
    try {
      await invoke("reveal_installation_location", { installationId: record.id, location });
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function loadLocalBackups(record: InstallRecord) {
    setLocalBackupStatus((current) => ({ ...current, [record.id]: "loading" }));
    try {
      const records = await invoke<BackupRecord[]>("list_backups", { installationId: record.id });
      setBackups((current) => ({ ...current, [record.id]: records }));
      setLocalBackupStatus((current) => ({ ...current, [record.id]: "loaded" }));
    } catch (reason) {
      requestedLocalBackups.current.delete(record.id);
      setLocalBackupStatus((current) => ({ ...current, [record.id]: "error" }));
      setError(String(reason));
    }
  }

  async function createBackup(record: InstallRecord) {
    setBackupBusyId(record.id);
    setBackupNotice(undefined);
    setError(undefined);
    try {
      const backup = await invoke<BackupRecord>("create_backup", { installationId: record.id });
      setBackups((current) => ({
        ...current,
        [record.id]: [backup, ...(current[record.id] ?? []).filter((item) => item.id !== backup.id)],
      }));
      setLocalBackupStatus((current) => ({ ...current, [record.id]: "loaded" }));
      setBackupNotice(`${record.tag} 백업을 만들었습니다.`);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBackupBusyId(undefined);
    }
  }

  async function revealBackup(backup: BackupRecord) {
    try {
      await invoke("reveal_backup", { backupId: backup.id });
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function exportBackup(backup: BackupRecord) {
    setBackupNotice(undefined);
    try {
      const exported = await invoke<boolean>("export_backup", { backupId: backup.id });
      if (exported) setBackupNotice("백업 ZIP을 내보냈습니다.");
    } catch (reason) {
      setError(String(reason));
    }
  }

  function openWebdavDialog() {
    setWebdavForm({
      endpoint: webdavConnection?.endpoint ?? "",
      username: webdavConnection?.username ?? "",
      password: "",
      rootFolder: webdavConnection?.rootFolder ?? "CataclysmHub",
    });
    setWebdavDialogOpen(true);
  }

  async function testWebdavConnection() {
    setCloudBusy("test");
    setError(undefined);
    try {
      await invoke("test_webdav_connection", { input: webdavForm });
      setBackupNotice("WebDAV 서버에 정상적으로 연결했습니다.");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setCloudBusy(undefined);
    }
  }

  async function saveWebdavConnection() {
    setCloudBusy("save");
    setError(undefined);
    try {
      const connection = await invoke<WebDavConnection>("save_webdav_connection", { input: webdavForm });
      setWebdavConnection(connection);
      setRemoteBackups({ dda: [], bn: [] });
      setRemoteBackupStatus({ dda: "idle", bn: "idle" });
      requestedRemoteBackups.current.clear();
      setWebdavDialogOpen(false);
      setBackupNotice("WebDAV를 연결했습니다.");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setCloudBusy(undefined);
    }
  }

  async function disconnectWebdav() {
    if (!window.confirm("이 기기의 WebDAV 연결 정보와 저장된 비밀번호를 제거할까요? 원격 백업 파일은 삭제되지 않습니다.")) return;
    setCloudBusy("disconnect");
    setError(undefined);
    try {
      await invoke("disconnect_webdav");
      setWebdavConnection(undefined);
      setRemoteBackups({ dda: [], bn: [] });
      setRemoteBackupStatus({ dda: "idle", bn: "idle" });
      requestedRemoteBackups.current.clear();
      setWebdavDialogOpen(false);
      setBackupNotice("WebDAV 연결을 해제했습니다. 원격 백업은 그대로 유지됩니다.");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setCloudBusy(undefined);
    }
  }

  async function refreshRemoteBackups(game: GameId) {
    setCloudBusy(`list:${game}`);
    setRemoteBackupStatus((current) => ({ ...current, [game]: "loading" }));
    setError(undefined);
    try {
      const records = await invoke<RemoteBackupRecord[]>("list_remote_backups", { game });
      setRemoteBackups((current) => ({ ...current, [game]: records }));
      setRemoteBackupStatus((current) => ({ ...current, [game]: "loaded" }));
    } catch (reason) {
      setRemoteBackupStatus((current) => ({ ...current, [game]: "error" }));
      setError(String(reason));
    } finally {
      setCloudBusy(undefined);
    }
  }

  async function uploadBackup(backup: BackupRecord) {
    setCloudBusy(`upload:${backup.id}`);
    setError(undefined);
    try {
      const remote = await invoke<RemoteBackupRecord>("upload_backup", { backupId: backup.id });
      setRemoteBackups((current) => ({
        ...current,
        [remote.game]: [remote, ...current[remote.game].filter((item) => item.fileName !== remote.fileName)],
      }));
      setRemoteBackupStatus((current) => ({ ...current, [remote.game]: "loaded" }));
      setBackupNotice("백업 ZIP을 WebDAV에 업로드했습니다.");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setCloudBusy(undefined);
    }
  }

  async function restoreRemoteBackup(record: InstallRecord, backup: RemoteBackupRecord) {
    setRestoreConfirmation(undefined);
    setCloudBusy(`restore:${backup.fileName}`);
    setError(undefined);
    try {
      const safetyBackup = await invoke<BackupRecord>("restore_remote_backup", {
        installationId: record.id,
        fileName: backup.fileName,
      });
      setBackups((current) => ({
        ...current,
        [record.id]: [safetyBackup, ...(current[record.id] ?? []).filter((item) => item.id !== safetyBackup.id)],
      }));
      setLocalBackupStatus((current) => ({ ...current, [record.id]: "loaded" }));
      setBackupNotice("클라우드 백업을 복원했습니다. 이전 데이터는 로컬 백업으로 보관했습니다.");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setCloudBusy(undefined);
    }
  }

  useEffect(() => {
    if (!currentInstallation || requestedLocalBackups.current.has(currentInstallation.id)) return;
    requestedLocalBackups.current.add(currentInstallation.id);
    void loadLocalBackups(currentInstallation);
  }, [currentInstallation?.id]);

  useEffect(() => {
    if (!currentInstallation || !webdavConnection || cloudBusy || requestedRemoteBackups.current.has(currentInstallation.game)) return;
    requestedRemoteBackups.current.add(currentInstallation.game);
    void refreshRemoteBackups(currentInstallation.game);
  }, [cloudBusy, currentInstallation?.game, webdavConnection]);

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="app-brand">
          <span className="brand-symbol">CH</span>
          <strong>Cataclysm Hub</strong>
        </div>

        <section className="sidebar-group" aria-labelledby="games-label">
          <span className="sidebar-label" id="games-label">게임</span>
          <div className="game-list">
            {games.map((game) => (
              <button
                className={!settingsOpen && activeGame === game.id ? "game-item active" : "game-item"}
                key={game.id}
                aria-pressed={!settingsOpen && activeGame === game.id}
                aria-label={`${game.subtitle} (${game.label})`}
                onClick={() => {
                  setActiveGame(game.id);
                  setSettingsOpen(false);
                  setVersionDialogOpen(false);
                  setExpandedReleaseKey(undefined);
                }}
              >
                <span>{game.label}</span>
              </button>
            ))}
          </div>
        </section>

        <nav className="sidebar-footer" aria-label="앱 설정">
          <button
            className={settingsOpen ? "settings-entry active" : "settings-entry"}
            aria-current={settingsOpen ? "page" : undefined}
            onClick={() => {
              setSettingsOpen(true);
              setVersionDialogOpen(false);
              setExpandedReleaseKey(undefined);
            }}
          >
            <SettingsIcon /> 설정
          </button>
        </nav>
      </aside>

      {settingsOpen ? (
        <section className="main-workspace settings-workspace">
          <header className="content-header">
            <div>
              <span>Cataclysm Hub</span>
              <h1>설정</h1>
            </div>
          </header>

          <div className="content-scroller settings-scroller">
            <section className="settings-card" aria-labelledby="webdav-settings-title">
              <header className="settings-card-header">
                <div>
                  <h2 id="webdav-settings-title">WebDAV</h2>
                  <p>백업 업로드와 복원에 사용할 WebDAV 서버를 관리합니다.</p>
                </div>
                <button className={webdavConnection ? "button secondary" : "button primary"} onClick={openWebdavDialog}>
                  {webdavConnection ? "연결 관리" : "WebDAV 연결"}
                </button>
              </header>

              {webdavConnection ? (
                <div className="global-connection">
                  <span className="connection-dot" />
                  <div>
                    <strong>연결됨</strong>
                    <span>{webdavConnection.username}</span>
                    <code title={`${webdavConnection.endpoint}${webdavConnection.rootFolder}/`}>{webdavConnection.endpoint}{webdavConnection.rootFolder}/</code>
                  </div>
                </div>
              ) : (
                <div className="global-connection disconnected">
                  <span className="connection-dot" />
                  <div>
                    <strong>연결되지 않음</strong>
                    <span>백업 업로드와 복원을 사용하려면 WebDAV 서버를 연결하세요.</span>
                  </div>
                </div>
              )}

              <p className="settings-note">비밀번호는 운영체제의 보안 자격 증명 저장소에 보관됩니다.</p>
            </section>
          </div>
        </section>
      ) : (
        <section className="main-workspace">
          <header className="content-header">
            <div>
              <span>{activeGameInfo.label}</span>
              <h1>{activeGameInfo.subtitle}</h1>
            </div>
            <div className="content-actions">
              <button className="button primary" onClick={() => setVersionDialogOpen(true)}>
                <DownloadIcon /> 설치
              </button>
            </div>
          </header>

          <div className="content-scroller" key={activeGame}>
            {!currentInstallation ? (
              <section className="empty-state" aria-labelledby="installations-empty-title">
                <h2 id="installations-empty-title">설치가 필요합니다</h2>
                <p>원하는 버전을 선택해 바로 설치할 수 있습니다.</p>
                <button className="button primary" onClick={() => setVersionDialogOpen(true)}><DownloadIcon /> 설치</button>
              </section>
            ) : (
              <div className="game-dashboard">
                <section className="launch-card" aria-labelledby="current-version-title">
                  <div className="launch-card-copy">
                    <span className="dashboard-eyebrow">현재 버전</span>
                    <h2 id="current-version-title" title={currentInstallation.tag}>{formatInstalledVersion(currentInstallation)}</h2>
                    <div className="update-state" aria-live="polite">
                      {loading && gameReleases.length === 0 ? (
                        <span className="checking"><RefreshIcon /> 업데이트 확인 중</span>
                      ) : updateRelease ? (
                        <span className="available">
                          <i /> 업데이트 가능
                          <strong>{releaseChannel(updateRelease, activeGame)} · {formatVersionTag(updateRelease.tag)}</strong>
                        </span>
                      ) : latestChannelRelease ? (
                        <span className="current"><CheckIcon /> 최신 상태</span>
                      ) : (
                        <span className="unavailable">업데이트 확인 불가</span>
                      )}
                    </div>
                    {updateProgress && (
                      <div className="update-progress" aria-label={`업데이트 ${updatePercent}%`}>
                        <div className="progress-track"><i style={{ width: `${updatePercent}%` }} /></div>
                      </div>
                    )}
                  </div>
                  <div className="launch-actions">
                    {updateRelease && (
                      <button className="button secondary" disabled={Boolean(updateProgress)} onClick={() => void install(updateRelease)}>
                        <DownloadIcon />
                        {updateProgress
                          ? updateProgress.stage === "unpacking" ? "설치 중" : `${updatePercent}%`
                          : "업데이트"}
                      </button>
                    )}
                    <button className="button primary launch-button" onClick={() => void launch(currentInstallation)}><PlayIcon /> 실행</button>
                  </div>
                </section>

                <section className="dashboard-card folder-card" aria-labelledby="folders-title">
                  <header className="dashboard-card-header">
                    <h2 id="folders-title">폴더</h2>
                  </header>
                  <div className="folder-actions">
                    <button onClick={() => void openLocation(currentInstallation, "install")}><FolderIcon /><span>설치</span></button>
                    <button onClick={() => void openLocation(currentInstallation, "config")}><FolderIcon /><span>설정</span></button>
                    <button onClick={() => void openLocation(currentInstallation, "save")}><FolderIcon /><span>세이브</span></button>
                  </div>
                </section>

                <div className="backup-grid">
                  <section className="dashboard-card backup-card" aria-labelledby="local-backups-title">
                    <header className="dashboard-card-header">
                      <h2 id="local-backups-title">로컬 백업</h2>
                      <button className="button secondary" disabled={backupBusyId === currentInstallation.id} onClick={() => void createBackup(currentInstallation)}>
                        <ArchiveIcon /> {backupBusyId === currentInstallation.id ? "백업 중" : "백업 만들기"}
                      </button>
                    </header>
                    <div className="dashboard-card-body">
                      {currentLocalBackupStatus === "loading" || currentLocalBackupStatus === "idle" ? (
                        <p className="section-empty">불러오는 중…</p>
                      ) : currentLocalBackupStatus === "error" ? (
                        <p className="section-empty">목록을 불러오지 못했습니다.</p>
                      ) : currentLocalBackups.length === 0 ? (
                        <p className="section-empty">백업이 없습니다.</p>
                      ) : (
                        <div className="backup-list">
                          {currentLocalBackups.slice(0, 3).map((backup) => (
                            <div className="backup-row" key={backup.id}>
                              <div className="backup-row-details">
                                <div className="backup-row-heading"><span className="backup-source local">내 기기</span><strong>백업 생성</strong></div>
                                <span className="backup-row-meta">{formatDateTime(backup.createdAt)} · {formatBytes(backup.size)}</span>
                              </div>
                              <div className="inline-actions">
                                <button onClick={() => void revealBackup(backup)}>위치 보기</button>
                                <button onClick={() => void exportBackup(backup)}>내보내기</button>
                                {webdavConnection && (
                                  <button disabled={cloudBusy === `upload:${backup.id}`} onClick={() => void uploadBackup(backup)}>
                                    {cloudBusy === `upload:${backup.id}` ? "업로드 중" : "업로드"}
                                  </button>
                                )}
                              </div>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  </section>

                  <section className="dashboard-card backup-card" aria-labelledby="cloud-backups-title">
                    <header className="dashboard-card-header">
                      <div className="dashboard-card-title">
                        <h2 id="cloud-backups-title">클라우드 백업</h2>
                        {webdavConnection && <span className="connection-status"><i /> 연결됨</span>}
                      </div>
                      {webdavConnection && (
                        <button className="button secondary" disabled={cloudBusy === `list:${activeGame}`} onClick={() => void refreshRemoteBackups(activeGame)}>
                          <RefreshIcon /> {cloudBusy === `list:${activeGame}` ? "불러오는 중" : "새로고침"}
                        </button>
                      )}
                    </header>
                    <div className="dashboard-card-body">
                      {!webdavConnection ? (
                        <div className="cloud-disconnected">
                          <span>연결되지 않음</span>
                          <button className="button secondary" onClick={() => setSettingsOpen(true)}><SettingsIcon /> WebDAV 설정</button>
                        </div>
                      ) : remoteBackupStatus[activeGame] === "loading" || remoteBackupStatus[activeGame] === "idle" ? (
                        <p className="section-empty">불러오는 중…</p>
                      ) : remoteBackupStatus[activeGame] === "error" ? (
                        <p className="section-empty">목록을 불러오지 못했습니다.</p>
                      ) : remoteBackups[activeGame].length === 0 ? (
                        <p className="section-empty">원격 백업이 없습니다.</p>
                      ) : (
                        <div className="remote-backup-list">
                          {remoteBackups[activeGame].map((remote) => (
                            <div className="remote-backup-row" key={remote.fileName}>
                              <div className="remote-backup-details">
                                <div className="remote-backup-heading"><span className="backup-source cloud">WebDAV</span><strong title={remote.fileName}>{remote.fileName}</strong></div>
                                <span className="remote-backup-meta">업로드됨: {formatDateTime(remote.modifiedAt ?? "")} · {formatBytes(remote.size)}</span>
                              </div>
                              <button className="text-button" disabled={cloudBusy === `restore:${remote.fileName}`} onClick={() => setRestoreConfirmation({ installation: currentInstallation, backup: remote })}>
                                {cloudBusy === `restore:${remote.fileName}` ? "복원 중" : "복원"}
                              </button>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  </section>
                </div>
              </div>
            )}
          </div>
        </section>
      )}

      {versionDialogOpen && (
        <div className="dialog-backdrop version-dialog-backdrop" role="presentation" onMouseDown={() => setVersionDialogOpen(false)}>
          <section className="version-dialog" role="dialog" aria-modal="true" aria-labelledby="version-dialog-title" onMouseDown={(event) => event.stopPropagation()}>
            <header>
              <div>
                <span>{activeGameInfo.label}</span>
                <h2 id="version-dialog-title">{activeGameInfo.subtitle} 설치</h2>
                <p>설치할 버전을 선택하세요.</p>
              </div>
              <button className="icon-button" aria-label="설치 닫기" onClick={() => setVersionDialogOpen(false)}><CloseIcon /></button>
            </header>

            <div className="version-dialog-toolbar">
              <div className="release-filters" role="group" aria-label="버전 유형 필터">
                {(["all", "stable", "experimental"] as Filter[]).map((item) => (
                  <button className={filter === item ? "selected" : ""} onClick={() => setFilter(item)} key={item}>
                    {item === "all" ? "전체" : item === "stable" ? "Stable" : activeGame === "bn" ? "Nightly" : "Experimental"}
                  </button>
                ))}
              </div>
              <button
                className={loading ? "button secondary is-loading" : "button secondary"}
                onClick={() => {
                  requestedPages.current.delete(`${activeGame}:1`);
                  void loadPage(activeGame, 1, true);
                }}
                disabled={loading}
              >
                <RefreshIcon /> 새로고침
              </button>
            </div>

            <div className="version-dialog-content">
              <section className="list-surface releases-surface" aria-label="설치 가능한 버전 목록">
                <div className="list-head release-grid" aria-hidden="true">
                  <span>채널</span><span>버전</span><span>작업</span>
                </div>
                <div className="release-list">
                  {visibleReleases.map((release) => {
                    const progress = installing[release.id];
                    const isInstalled = installedReleaseIds.has(release.id);
                    const releaseKind = release.prerelease ? activeGame === "bn" ? "Nightly" : "Experimental" : "Stable";
                    const releaseKey = `${activeGame}:${release.id}`;
                    const changelogOpen = expandedReleaseKey === releaseKey;
                    return (
                      <article className="release-item" key={release.id}>
                        <div className="release-row release-grid">
                          <span className={release.prerelease ? "release-kind experimental" : "release-kind stable"}>{releaseKind}</span>
                          <div className="primary-cell">
                            <strong title={release.tag}>{formatVersionTag(release.tag)}</strong>
                            <span>{formatDate(release.publishedAt)}{release.recommendedAsset && ` · ${formatBytes(release.recommendedAsset.size)}`}</span>
                          </div>
                          <div className="row-actions release-actions">
                            <button
                              className={changelogOpen ? "text-button open" : "text-button"}
                              aria-controls={`changelog-${release.id}`}
                              aria-expanded={changelogOpen}
                              onClick={() => setExpandedReleaseKey((current) => current === releaseKey ? undefined : releaseKey)}
                            >
                              변경사항 <ChevronDownIcon />
                            </button>
                            {isInstalled ? (
                              <span className="status-label success"><CheckIcon /> 설치됨</span>
                            ) : !release.recommendedAsset ? (
                              <span className="status-label muted">사용 불가</span>
                            ) : progress ? (
                              <div className="progress-wrap">
                                <span>{progress.stage === "unpacking" ? "설치 중" : `${Math.round((progress.receivedBytes / Math.max(progress.totalBytes, 1)) * 100)}%`}</span>
                                <div className="progress-track"><i style={{ width: `${Math.min(100, (progress.receivedBytes / Math.max(progress.totalBytes, 1)) * 100)}%` }} /></div>
                              </div>
                            ) : (
                              <button className="button secondary install-button" onClick={() => void install(release)}><DownloadIcon /> 설치</button>
                            )}
                          </div>
                        </div>
                        {changelogOpen && (
                          <section className="changelog" id={`changelog-${release.id}`} aria-label={`${release.tag} 변경사항`}>
                            <h3>변경사항</h3>
                            <div className="changelog-body">{release.body?.trim() || "이 버전에는 작성된 변경사항이 없습니다."}</div>
                          </section>
                        )}
                      </article>
                    );
                  })}
                  {!loading && gameReleases.length > 0 && visibleReleases.length === 0 && <div className="list-message">이 조건에 맞는 버전이 없습니다.</div>}
                  {!loading && gameReleases.length === 0 && <div className="list-message">표시할 수 있는 버전이 없습니다.</div>}
                  {loading && <div className="list-message">버전을 불러오는 중…</div>}
                </div>
                {!loading && hasMore[activeGame] && (
                  <button className="load-more" onClick={() => void loadPage(activeGame, page[activeGame] + 1)}>이전 버전 100개 더 보기</button>
                )}
                {page[activeGame] >= 10 && <p className="release-limit">최근 1,000개 버전까지 표시합니다.</p>}
              </section>
            </div>
          </section>
        </div>
      )}

      {webdavDialogOpen && (
        <div className="dialog-backdrop" role="presentation" onMouseDown={() => !cloudBusy && setWebdavDialogOpen(false)}>
          <section className="webdav-dialog" role="dialog" aria-modal="true" aria-labelledby="webdav-dialog-title" onMouseDown={(event) => event.stopPropagation()}>
            <header>
              <div><h2 id="webdav-dialog-title">WebDAV 연결</h2><p>개인 WebDAV 서버에 백업을 보관합니다.</p></div>
              <button className="icon-button" aria-label="WebDAV 설정 닫기" disabled={Boolean(cloudBusy)} onClick={() => setWebdavDialogOpen(false)}><CloseIcon /></button>
            </header>
            <form onSubmit={(event) => { event.preventDefault(); void saveWebdavConnection(); }}>
              <label>서버 URL
                <input autoFocus type="url" required autoComplete="url" placeholder="https://cloud.example.com/remote.php/dav/files/name/" value={webdavForm.endpoint} onChange={(event) => setWebdavForm((current) => ({ ...current, endpoint: event.target.value }))} />
              </label>
              <div className="webdav-form-row">
                <label>사용자명
                  <input required autoComplete="username" value={webdavForm.username} onChange={(event) => setWebdavForm((current) => ({ ...current, username: event.target.value }))} />
                </label>
                <label>원격 폴더
                  <input required value={webdavForm.rootFolder} onChange={(event) => setWebdavForm((current) => ({ ...current, rootFolder: event.target.value }))} />
                </label>
              </div>
              <label>앱 비밀번호
                <input type="password" required autoComplete="current-password" placeholder="WebDAV 앱 비밀번호" value={webdavForm.password} onChange={(event) => setWebdavForm((current) => ({ ...current, password: event.target.value }))} />
              </label>
              <p className="webdav-security-note">비밀번호는 운영체제의 보안 자격 증명 저장소에만 저장됩니다.</p>
              {webdavForm.endpoint.trim().toLowerCase().startsWith("http://") && <p className="webdav-http-warning">HTTP 연결은 비밀번호와 백업 데이터를 암호화하지 않습니다.</p>}
              <div className="dialog-actions">
                {webdavConnection && <button className="dialog-danger" type="button" disabled={Boolean(cloudBusy)} onClick={() => void disconnectWebdav()}>연결 해제</button>}
                <button className="button secondary test-connection" type="button" disabled={Boolean(cloudBusy)} onClick={() => void testWebdavConnection()}>{cloudBusy === "test" ? "확인 중" : "연결 테스트"}</button>
                <button className="button primary" type="submit" disabled={Boolean(cloudBusy)}>{cloudBusy === "save" ? "저장 중" : "저장"}</button>
              </div>
            </form>
          </section>
        </div>
      )}

      {restoreConfirmation && (
        <div className="dialog-backdrop" role="presentation" onMouseDown={() => setRestoreConfirmation(undefined)}>
          <section className="webdav-dialog restore-dialog" role="dialog" aria-modal="true" aria-labelledby="restore-dialog-title" onMouseDown={(event) => event.stopPropagation()}>
            <header>
              <div>
                <h2 id="restore-dialog-title">클라우드 백업 복원</h2>
                <p>{games.find((game) => game.id === restoreConfirmation.installation.game)?.label} 데이터를 교체합니다.</p>
              </div>
              <button className="icon-button" aria-label="복원 취소" onClick={() => setRestoreConfirmation(undefined)}><CloseIcon /></button>
            </header>
            <p className="restore-warning">
              현재 설정과 세이브를 <strong>{restoreConfirmation.backup.fileName}</strong> 백업으로 교체합니다. 복원 전 현재 데이터는 자동으로 로컬 백업됩니다.
            </p>
            <div className="dialog-actions">
              <button autoFocus className="button secondary" type="button" onClick={() => setRestoreConfirmation(undefined)}>취소</button>
              <button className="button primary" type="button" onClick={() => void restoreRemoteBackup(restoreConfirmation.installation, restoreConfirmation.backup)}>복원 시작</button>
            </div>
          </section>
        </div>
      )}

      {error && <div className="notice error" role="alert"><div><b>작업을 완료하지 못했어요</b><span>{error}</span></div><button aria-label="알림 닫기" onClick={() => setError(undefined)}><CloseIcon /></button></div>}
      {backupNotice && <div className="notice success" role="status"><div><b>{backupNotice}</b></div><button aria-label="알림 닫기" onClick={() => setBackupNotice(undefined)}><CloseIcon /></button></div>}
    </main>
  );
}
