// Ratings are stored from 0 to 10 and shown as five stars, in halves.

const MAX_RATING = 10;

/** The rating at a place along the stars: the left half of a star is that star less a half. */
export function ratingAt(x: number, width: number): number {
  if (width <= 0) return 1;
  return Math.max(1, Math.min(MAX_RATING, Math.ceil((x / width) * MAX_RATING)));
}

/** One half-star up or down. Going down from the lowest clears the rating. */
export function stepRating(rating: number | null, step: number): number | null {
  const next = (rating ?? 0) + step;
  return next <= 0 ? null : Math.min(next, MAX_RATING);
}

export function starsLabel(rating: number | null): string {
  if (rating === null) return "Not rated";
  const stars = rating / 2;
  return `${stars} ${stars === 1 ? "star" : "stars"}`;
}

/** The rating a number key gives selected rows: 1 to 5 stars, and 0 clears it. */
export function ratingForDigit(key: string): number | null | undefined {
  if (!/^[0-5]$/.test(key)) return undefined;
  return key === "0" ? null : Number(key) * 2;
}

export function formatPlays(track: { playCount: number; lastPlayedAt: number | null }): {
  count: string;
  title: string;
} {
  if (track.playCount === 0) return { count: "", title: "Not played yet" };
  const when = track.lastPlayedAt === null ? "" : `, last ${new Date(track.lastPlayedAt).toLocaleString()}`;
  return {
    count: track.playCount.toLocaleString(),
    title: `Played ${track.playCount.toLocaleString()} ${track.playCount === 1 ? "time" : "times"}${when}`,
  };
}
