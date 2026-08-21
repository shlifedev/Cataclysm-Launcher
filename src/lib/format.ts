export function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "–";
  const units = ["B", "KB", "MB", "GB"];
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  const amount = value / 1024 ** index;
  return `${amount >= 10 || index === 0 ? amount.toFixed(0) : amount.toFixed(1)} ${units[index]}`;
}

export function formatDate(value: string): string {
  return new Intl.DateTimeFormat("ko-KR", {
    year: "numeric",
    month: "short",
    day: "numeric",
  }).format(new Date(value));
}

export function gameTitle(game: "dda" | "bn"): string {
  return game === "dda" ? "Cataclysm: DDA" : "Cataclysm: BN";
}
