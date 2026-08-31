<script setup lang="ts">
defineProps<{
  /** Stay hidden until the row is hovered or something in it has focus.
   * Keyboard users still reach it — `focus-within` keeps it visible. */
  showOnHover?: boolean;
}>();

const base =
  "absolute top-1 right-1 flex aspect-square w-5 items-center justify-center rounded-md text-slate-400 hover:bg-slate-200 hover:text-slate-700 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-slate-400 dark:hover:bg-slate-700 dark:hover:text-slate-200";
// `pointer-events-none` while hidden matters: an invisible but clickable
// button sitting over the row would eat clicks meant for the row itself.
// Focus is unaffected by it, so tabbing to the action still works.
const hover =
  "pointer-events-none opacity-0 transition-opacity group-focus-within/menu-item:pointer-events-auto group-focus-within/menu-item:opacity-100 group-hover/menu-item:pointer-events-auto group-hover/menu-item:opacity-100 focus-visible:opacity-100";
</script>

<template>
  <button :class="[base, showOnHover ? hover : '']">
    <slot />
  </button>
</template>
