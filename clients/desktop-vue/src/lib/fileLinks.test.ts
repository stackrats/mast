import { describe, expect, it } from "vite-plus/test";

import { splitFileLinks } from "./fileLinks";

describe("splitFileLinks", () => {
  it("links a trace frame with its parenthesised line", () => {
    const parts = splitFileLinks("#0 /var/www/html/app/Jobs/Ship.php(25): handle()");
    expect(parts).toEqual([
      { text: "#0 " },
      { text: "/var/www/html/app/Jobs/Ship.php(25)", file: "app/Jobs/Ship.php", line: 25 },
      { text: ": handle()" },
    ]);
  });

  it("links an exception header with its colon line", () => {
    const parts = splitFileLinks("thrown in /var/www/html/routes/web.php:12");
    expect(parts[1]).toEqual({
      text: "/var/www/html/routes/web.php:12",
      file: "routes/web.php",
      line: 12,
    });
  });

  it("a path without a line still links, a directory does not", () => {
    const withFile = splitFileLinks("wrote /var/www/html/storage/logs/laravel.log cleanly");
    expect(withFile[1].file).toBe("storage/logs/laravel.log");
    expect(withFile[1].line).toBeUndefined();
    // "storage/" alone is prose, not a destination.
    const dirOnly = splitFileLinks("check /var/www/html/storage for stale views");
    expect(dirOnly.every((p) => p.file === undefined)).toBe(true);
  });

  it("is lossless around multiple hits and plain text", () => {
    const text = "at /var/www/html/app/A.php(1) then /var/www/html/app/B.php:2 and that was all";
    expect(
      splitFileLinks(text)
        .map((p) => p.text)
        .join(""),
    ).toBe(text);
    expect(splitFileLinks("no paths here")).toEqual([{ text: "no paths here" }]);
    expect(splitFileLinks("")).toEqual([{ text: "" }]);
  });
});
