// Bounded append: drop oldest entries beyond `cap` (log/output backpressure
// on the UI side — plan §3 ring-buffer requirement).

export function pushBounded<T>(items: T[], item: T, cap: number): void {
  items.push(item);
  if (items.length > cap) {
    items.splice(0, items.length - cap);
  }
}
