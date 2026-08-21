export type GameId = "dda" | "bn";

export type ReleaseAsset = {
  id: number;
  name: string;
  size: number;
  url: string;
};

export type Release = {
  id: number;
  tag: string;
  name: string;
  prerelease: boolean;
  publishedAt: string;
  body?: string;
  recommendedAsset?: ReleaseAsset;
};

export type ReleasePage = {
  game: GameId;
  page: number;
  releases: Release[];
  hasMore: boolean;
  fromCache: boolean;
};

export type InstallRecord = {
  id: string;
  game: GameId;
  releaseId: number;
  tag: string;
  name: string;
  assetName: string;
  installedAt: string;
  installDir: string;
  userDir: string;
  executablePath: string;
};

export type InstallProgress = {
  releaseId: number;
  stage: "downloading" | "unpacking" | "complete" | "failed";
  receivedBytes: number;
  totalBytes: number;
  message: string;
};

export type BackupRecord = {
  id: string;
  installationId: string;
  game: GameId;
  releaseId: number;
  tag: string;
  createdAt: string;
  archivePath: string;
  size: number;
  contents: string[];
  schemaVersion: number;
};

export type WebDavConnection = {
  endpoint: string;
  username: string;
  rootFolder: string;
};

export type WebDavConnectionInput = {
  endpoint: string;
  username: string;
  password: string;
  rootFolder: string;
};

export type RemoteBackupRecord = {
  fileName: string;
  game: GameId;
  size: number;
  modifiedAt?: string;
};
