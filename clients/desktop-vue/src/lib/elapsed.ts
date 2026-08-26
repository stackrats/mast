// A once-a-second clock for "how long has this been running" readouts, and
// the formatter that goes with it.
//
// The interval only exists while something is actually being timed: a rebuild
// is the long case (tens of minutes), and the rest of the time an app-wide
// timer would be re-rendering cards for nothing.

import { computed, onScopeDispose, ref, watch, type Ref } from "vue";

/** Milliseconds since `startedAt`, ticking while `startedAt` is non-null. */
export function useElapsed(startedAt: Ref<number | null>) {
  const now = ref(Date.now());
  let timer: ReturnType<typeof setInterval> | null = null;

  const stop = () => {
    if (timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  };

  watch(
    startedAt,
    (at) => {
      stop();
      if (at === null) return;
      // Set immediately so the readout starts at 0s rather than blank.
      now.value = Date.now();
      timer = setInterval(() => (now.value = Date.now()), 1000);
    },
    { immediate: true },
  );

  onScopeDispose(stop);

  return computed(() => (startedAt.value === null ? 0 : now.value - startedAt.value));
}

/** `9s`, `4m 12s`, `1h 06m` — the coarsest unit that still says something.
 * Seconds are dropped past an hour, where they are noise. */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours > 0) return `${hours}h ${String(minutes).padStart(2, "0")}m`;
  if (minutes > 0) return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
  return `${seconds}s`;
}
