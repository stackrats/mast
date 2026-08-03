<template>
  <!-- Absolutely placed so a long group label never pushes it out of line. -->
  <button
    class="absolute top-2 right-2 flex aspect-square w-5 items-center justify-center rounded-md text-slate-400 outline-none hover:bg-slate-200 hover:text-slate-700 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-slate-400 dark:hover:bg-slate-700 dark:hover:text-slate-200"
  >
    <slot />
  </button>
</template>
