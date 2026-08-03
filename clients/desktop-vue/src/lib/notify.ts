// Native notifications (M8): fired only for enabled categories AND while the
// window is hidden (close-to-tray) — a visible app already shows the state.

import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

import { loadNotificationPrefs, type NotificationPrefs } from "./prefs";

let permitted: boolean | null = null;

export async function notify(
  category: keyof NotificationPrefs,
  title: string,
  body: string,
): Promise<void> {
  if (!document.hidden) return;
  if (!loadNotificationPrefs()[category]) return;
  try {
    if (permitted === null) {
      permitted = await isPermissionGranted();
      if (!permitted) permitted = (await requestPermission()) === "granted";
    }
    if (permitted) sendNotification({ title, body });
  } catch {
    // Notifications are best-effort; never let them break state handling.
  }
}
