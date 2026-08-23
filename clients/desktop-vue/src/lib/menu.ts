// Shared reka-ui menu styling: menubar, dropdowns and select all read as one
// component family.

export const menuContentClass =
  "z-50 min-w-36 rounded-md border border-slate-200 bg-white p-1 shadow-lg dark:border-slate-700 dark:bg-slate-900";

export const menuItemClass =
  "flex cursor-default items-center gap-2 rounded px-2 py-1.5 text-xs text-slate-700 outline-none data-[disabled]:opacity-40 data-highlighted:bg-slate-100 dark:text-slate-200 dark:data-highlighted:bg-slate-800";

export const menuSeparatorClass = "my-1 h-px bg-slate-100 dark:bg-slate-800";

/** Inline icon-button recipe for dense rows — `Button size="iconSm"` is
 * 24px, which would set the row height. */
export const iconButtonClass =
  "rounded p-0.5 text-slate-400 hover:bg-slate-200 hover:text-slate-700 dark:hover:bg-slate-700 dark:hover:text-slate-200";
