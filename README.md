# Cataclysm Hub

macOS와 Windows에서 Cataclysm: Dark Days Ahead와 Cataclysm: Bright Nights의 버전을 선택해 설치·실행하는 Tauri 데스크톱 런처입니다.

## 제공 기능

- CDDA/CBN 전환, stable 및 experimental/nightly 버전 탐색
- GitHub Releases API 기준 최신 1,000개 릴리즈 조회와 100개 단위 추가 로드
- 현재 운영체제와 CPU에 맞는 그래픽 타일·사운드팩 빌드 자동 선택(macOS DMG, Windows x64 ZIP)
- 게임별 단일 활성 버전과 이전 버전의 재다운로드를 피하는 앱 캐시
- 설치 진행률, 현재 버전 실행, Finder 또는 Windows 파일 탐색기에서 설치 폴더 보기
- 설치본별 설정·세이브 폴더 열기와 `config/`·`save/` ZIP 스냅샷 백업
- 런처 관리 백업의 파일 위치 표시 및 원하는 위치로 ZIP 내보내기
- HTTP/HTTPS WebDAV 서버 연결, 로컬 백업 ZIP 업로드, 원격 백업 목록 및 안전 복원

현재 선택한 버전만 설치된 게임으로 표시됩니다. 이전에 내려받아 푼 버전은 앱 데이터의 `installs` 폴더에 캐시되어 다시 선택할 때 즉시 전환됩니다. 기본 앱 데이터 위치는 macOS의 `~/Library/Application Support/gg.cataclysm.hub`, Windows의 `%APPDATA%\gg.cataclysm.hub`입니다. 게임 데이터는 그 아래 `userdata/dda` 또는 `userdata/bn`에 보관되어 CDDA와 CBN 사이에서만 분리됩니다. 관리형 백업은 `backups` 폴더와 `backups.json`에 보관됩니다.

## WebDAV 클라우드 백업

왼쪽 아래 `설정`에서 WebDAV 서버를 연결하고 관리합니다. Nextcloud, ownCloud, Synology WebDAV 및 표준 WebDAV 개인 서버처럼 HTTP 또는 HTTPS와 Basic 인증(권장: 서비스의 앱 비밀번호)을 제공하는 서버를 지원합니다. HTTP는 전송 내용을 암호화하지 않으므로 신뢰하는 내부망에서만 사용해야 합니다.

- 서버 URL은 WebDAV 루트 URL을 입력하고, 원격 폴더는 기본값 `CataclysmHub` 또는 원하는 하위 폴더로 설정합니다.
- DDA와 BN 백업은 원격의 각각 `dda/`, `bn/` 폴더에 보관됩니다.
- 비밀번호는 앱 데이터 파일이 아닌 macOS 키체인 또는 Windows 자격 증명 관리자에만 저장됩니다.
- 복원은 선택한 게임의 공용 `config/`·`save/`를 교체하기 전에 현재 데이터를 로컬 ZIP으로 자동 백업합니다.
- 원격 삭제와 자동 동기화는 지원하지 않습니다. 업로드와 원격 목록 갱신은 사용자가 직접 실행합니다.

## 개발

```sh
npm ci
npm run tauri dev
```

Windows 개발에는 Tauri의 Windows 사전 요구 사항인 Microsoft C++ Build Tools와 WebView2가 필요합니다. macOS 앱/DMG는 기본 번들 명령으로 빌드합니다.

```sh
npm run tauri build
```

Windows 배포본은 설치 과정 없이 바로 실행할 수 있는 단일 portable EXE로 빌드합니다.

```sh
npm run tauri -- build --no-bundle
```

## CI 및 프리릴리스 배포

GitHub Actions는 `main` 대상 Pull Request와 `main` 푸시에서 macOS와 Windows를 각각 사용해 프런트엔드 빌드, Rust 포맷 검사, Clippy, 단위 테스트를 수행합니다. 로컬에서도 CI와 같은 검증을 다음 순서로 실행할 수 있습니다.

```sh
npm ci
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

macOS Universal 2 DMG와 Windows x64 portable EXE 프리릴리스는 `main`에 포함된 커밋에 정확한 `vX.Y.Z` 형식의 태그를 푸시하면 함께 생성됩니다. 태그를 만들기 전에 `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`의 버전을 모두 같은 `X.Y.Z`로 맞춥니다.

```sh
git checkout main
git pull --ff-only
npm run check:release-version -- vX.Y.Z
git tag vX.Y.Z
git push origin vX.Y.Z
```

태그 작업은 Apple Silicon과 Intel Mac에서 모두 동작하는 Universal 2 DMG, 설치 없이 실행하는 Windows x64 portable EXE와 각각의 SHA-256 체크섬 파일을 GitHub Prerelease에 올립니다. macOS에서는 내려받은 파일과 체크섬을 같은 폴더에 둔 뒤 다음처럼 확인합니다.

```sh
shasum -a 256 -c '다운로드한-DMG-파일명.dmg.sha256'
```

Windows에서는 PowerShell의 출력과 함께 제공된 `.sha256` 파일의 해시가 같은지 확인합니다.

```powershell
(Get-FileHash '.\다운로드한-설치파일.exe' -Algorithm SHA256).Hash.ToLower()
```

이 배포본은 테스트용으로 **서명 및 공증되지 않습니다**. 따라서 macOS Gatekeeper 또는 Windows SmartScreen이 처음 실행할 때 경고하거나 실행을 차단할 수 있습니다. 신뢰할 수 있는 GitHub Prerelease에서 받은 파일인지와 SHA-256 검증 결과를 확인한 경우에만 사용하세요.

GitHub API는 레포지토리별 가장 최근 1,000개 릴리즈만 반환하므로, 런처도 그 범위까지만 표시합니다.
