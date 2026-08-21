# Cataclysm Hub

macOS에서 Cataclysm: Dark Days Ahead와 Cataclysm: Bright Nights의 버전을 선택해 설치·실행하는 Tauri 데스크톱 런처입니다.

## 제공 기능

- CDDA/CBN 전환, stable 및 experimental/nightly 버전 탐색
- GitHub Releases API 기준 최신 1,000개 릴리즈 조회와 100개 단위 추가 로드
- 현재 Mac에 맞는 그래픽 타일·사운드팩 DMG 자동 선택
- 게임별 단일 활성 버전과 이전 버전의 재다운로드를 피하는 앱 캐시
- 설치 진행률, 현재 버전 실행, Finder에서 설치 폴더 보기
- 설치본별 설정·세이브 폴더 열기와 `config/`·`save/` ZIP 스냅샷 백업
- 런처 관리 백업의 Finder 표시 및 원하는 위치로 ZIP 내보내기
- HTTP/HTTPS WebDAV 서버 연결, 로컬 백업 ZIP 업로드, 원격 백업 목록 및 안전 복원

현재 선택한 버전만 설치된 게임으로 표시됩니다. 이전에 내려받아 푼 버전은 `~/Library/Application Support/gg.cataclysm.hub/installs`에 캐시되어 다시 선택할 때 즉시 전환됩니다. 게임 데이터는 `userdata/dda` 또는 `userdata/bn`에 보관되어 CDDA와 CBN 사이에서만 분리됩니다. 관리형 백업은 `backups` 폴더와 `backups.json`에 보관됩니다.

## WebDAV 클라우드 백업

왼쪽 아래 `설정`에서 WebDAV 서버를 연결하고 관리합니다. Nextcloud, ownCloud, Synology WebDAV 및 표준 WebDAV 개인 서버처럼 HTTP 또는 HTTPS와 Basic 인증(권장: 서비스의 앱 비밀번호)을 제공하는 서버를 지원합니다. HTTP는 전송 내용을 암호화하지 않으므로 신뢰하는 내부망에서만 사용해야 합니다.

- 서버 URL은 WebDAV 루트 URL을 입력하고, 원격 폴더는 기본값 `CataclysmHub` 또는 원하는 하위 폴더로 설정합니다.
- DDA와 BN 백업은 원격의 각각 `dda/`, `bn/` 폴더에 보관됩니다.
- 비밀번호는 앱 데이터 파일이 아닌 macOS 키체인에만 저장됩니다.
- 복원은 선택한 게임의 공용 `config/`·`save/`를 교체하기 전에 현재 데이터를 로컬 ZIP으로 자동 백업합니다.
- 원격 삭제와 자동 동기화는 지원하지 않습니다. 업로드와 원격 목록 갱신은 사용자가 직접 실행합니다.

## 개발

```sh
npm install
npm run tauri dev
```

배포용 macOS 앱은 다음 명령으로 빌드합니다.

```sh
npm run tauri build
```

GitHub API는 레포지토리별 가장 최근 1,000개 릴리즈만 반환하므로, 런처도 그 범위까지만 표시합니다.
