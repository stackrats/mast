import { describe, expect, it } from "vite-plus/test";

import type { ProjectCommand, ProjectSummary } from "../bindings";
import {
  domainFromName,
  isHostCommand,
  isLocalDomain,
  siteFix,
  siteOf,
  suggestCommandDomain,
} from "./localDomains";

function command(name: string, line: string, cwd: string | null = null): ProjectCommand {
  return {
    name,
    command: line,
    autoStart: false,
    cwd,
    after: null,
    readyWhen: null,
    autoRestart: false,
    restartWhenChanged: [],
    fromManifest: false,
  };
}

function project(name: string, localDomain: string | null): ProjectSummary {
  return {
    id: name,
    name,
    path: `/code/${name}`,
    status: "running",
    composeProjectName: name,
    isSail: true,
    services: [],
    resolutionError: null,
    warnings: [],
    localDomain,
  } as ProjectSummary;
}

describe("local domains", () => {
  it("accepts only names that cannot reach public DNS", () => {
    expect(isLocalDomain("api.shop.test")).toBe(true);
    expect(isLocalDomain("web.localhost")).toBe(true);
    expect(isLocalDomain("shop.dev")).toBe(false);
    expect(isLocalDomain("-shop.test")).toBe(false);
    expect(isLocalDomain("sh_op.test")).toBe(false);
    expect(isLocalDomain(".test")).toBe(false);
  });

  it("reads the site as the last two labels", () => {
    expect(siteOf("api.thinksolar.test")).toBe("thinksolar.test");
    expect(siteOf("api-thinksolar.test")).toBe("api-thinksolar.test");
    expect(siteOf("a.b.c.localhost")).toBe("c.localhost");
  });

  it("keeps a dev host's labels so siblings stay siblings", () => {
    expect(domainFromName("api.thinksolar.local")).toBe("api.thinksolar.test");
    expect(domainFromName("suite.thinksolar.local")).toBe("suite.thinksolar.test");
    expect(domainFromName("pdf-service.thinksolar.local")).toBe("pdf-service.thinksolar.test");
    // Not a dev host: one label, as before.
    expect(domainFromName("kusina.design")).toBe("kusina-design.test");
    expect(domainFromName("My App")).toBe("my-app.test");
    expect(domainFromName("")).toBe("app.test");
  });

  it("tells host commands from container ones", () => {
    expect(isHostCommand(command("web", "vp dev"))).toBe(true);
    expect(isHostCommand(command("dev", "sail artisan dev"))).toBe(false);
    expect(isHostCommand(command("dev", "  sail npm run dev"))).toBe(false);
    expect(isHostCommand(command("x", "sailor"))).toBe(true);
  });

  it("suggests a command domain on the app's site", () => {
    const frontend = command("dev frontend", "vp dev", "../suite.thinksolar.local");
    // A sibling of an app with a parent.
    expect(suggestCommandDomain(project("api", "api.thinksolar.test"), frontend)).toBe(
      "suite.thinksolar.test",
    );
    // The field case: a flat app name. The folder is a dev host, so its own
    // name — and the dialog then offers to line the app up with it.
    expect(suggestCommandDomain(project("api", "api-thinksolar.test"), frontend)).toBe(
      "suite.thinksolar.test",
    );
    // No folder to go on: under the app's own domain.
    expect(
      suggestCommandDomain(project("shop", "shop.test"), command("dev frontend", "vp dev")),
    ).toBe("dev-frontend.shop.test");
    // No app domain yet: from the project's name, as the app would get.
    expect(
      suggestCommandDomain(project("api.thinksolar.local", null), command("docs", "vp docs:dev")),
    ).toBe("docs.thinksolar.test");
  });

  it("offers both ways to put a command on the app's site", () => {
    expect(siteFix("api-thinksolar.test", "suite.thinksolar.test")).toEqual({
      commandDomain: "suite.api-thinksolar.test",
      appDomain: "api.thinksolar.test",
    });
    expect(siteFix("thinksolar-api.test", "suite.thinksolar.test")?.appDomain).toBe(
      "api.thinksolar.test",
    );
    expect(siteFix("my-thinksolar-api.test", "suite.thinksolar.test")?.appDomain).toBe(
      "my-api.thinksolar.test",
    );
    // Nothing of the frontend's site in the app's name: only the move.
    expect(siteFix("billing.test", "suite.thinksolar.test")).toEqual({
      commandDomain: "suite.billing.test",
      appDomain: null,
    });
    // Already one site, no app domain, or nothing valid typed yet.
    expect(siteFix("api.thinksolar.test", "suite.thinksolar.test")).toBeNull();
    expect(siteFix(null, "suite.thinksolar.test")).toBeNull();
    expect(siteFix("api.thinksolar.test", "suite")).toBeNull();
  });
});
