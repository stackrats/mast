import { describe, expect, it } from "vite-plus/test";

import { parseDeepLink } from "./deeplink";

describe("parseDeepLink", () => {
  it("a clone link carries its decoded repository URL", () => {
    expect(parseDeepLink("mast://clone?url=git%40github.com%3Aacme%2Fshop.git")).toEqual({
      kind: "clone",
      url: "git@github.com:acme/shop.git",
    });
    expect(parseDeepLink("mast://clone?url=https://github.com/acme/shop.git")).toEqual({
      kind: "clone",
      url: "https://github.com/acme/shop.git",
    });
  });

  it("a project link selects by name or path suffix", () => {
    expect(parseDeepLink("mast://project/kusina")).toEqual({ kind: "project", ref: "kusina" });
    // Path suffixes keep their slashes.
    expect(parseDeepLink("mast://project/code%2Fshop")).toEqual({
      kind: "project",
      ref: "code/shop",
    });
  });

  /// Unknown actions and malformed links are ignored, never guessed at — a
  /// registered scheme is reachable by any webpage.
  it("everything else is null", () => {
    expect(parseDeepLink("mast://start?project=x")).toBeNull();
    expect(parseDeepLink("mast://clone")).toBeNull();
    expect(parseDeepLink("mast://clone?url=")).toBeNull();
    expect(parseDeepLink("mast://project")).toBeNull();
    expect(parseDeepLink("mast://project/%zz")).toBeNull();
    expect(parseDeepLink("https://mast.sh")).toBeNull();
    expect(parseDeepLink("")).toBeNull();
  });
});
