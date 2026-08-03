<script setup lang="ts">
// shadcn-vue's Sidebar menu primitives, vendored as the subset this app uses.
// The shell (SidebarProvider, the offcanvas Sheet, the collapsible rail) is
// deliberately not taken: it fixes width through a CSS variable, which is
// incompatible with this sidebar's drag-to-resize, and its mobile drawer is
// dead weight in a desktop window. See Sidebar.vue.
</script>

<template>
  <div class="relative flex w-full min-w-0 flex-col p-2">
    <slot />
  </div>
</template>
