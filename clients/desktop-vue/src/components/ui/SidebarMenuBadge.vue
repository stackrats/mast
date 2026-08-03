<template>
  <!-- Trailing count. `ml-auto` rather than absolute placement: it has to push
       against a truncating label, not overlap it. -->
  <span
    class="ml-auto flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full bg-sky-100 px-1.5 text-[10px] font-medium text-sky-700 tabular-nums select-none dark:bg-neutral-800 dark:text-neutral-300"
  >
    <slot />
  </span>
</template>
