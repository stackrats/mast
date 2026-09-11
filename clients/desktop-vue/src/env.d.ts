declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, unknown>;
  export default component;
}

// Vite's `?raw` suffix hands back a module's source as a string. Used by the
// banner test, which asserts on the SVG markup itself rather than on a
// rendered component.
declare module "*?raw" {
  const source: string;
  export default source;
}
