import { readonly, ref } from "vue";

// The size of the window, as something the UI can react to.
//
// One listener for the whole app rather than one per caller: the sidebar, the
// logs panel and the shell all ask the same question, and a listener each is
// a listener each to forget to remove. It is attached on first use and never
// detached — there is no point in the app's life where nothing wants to know
// how big the window is.
//
// This matters far more under a tiling manager than a floating one. There the
// window is resized by someone other than the user, at moments the user did
// not choose: opening an unrelated terminal retiles the workspace and every
// window in it changes width mid-frame.

const width = ref(0);
const height = ref(0);
let attached = false;

function measure() {
  width.value = window.innerWidth;
  height.value = window.innerHeight;
}

export function useViewport() {
  if (!attached && typeof window !== "undefined") {
    attached = true;
    window.addEventListener("resize", measure, { passive: true });
    measure();
  }
  return { width: readonly(width), height: readonly(height) };
}
