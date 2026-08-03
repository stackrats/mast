// Pure patch-application over the project list — the store's reducer for
// project-level events. Discovery/docker/watched-directory events are applied
// directly in the store.

import type { PatchEvent, ProjectSummary } from "../bindings";

export function applyPatchEvent(projects: ProjectSummary[], event: PatchEvent): ProjectSummary[] {
  switch (event.type) {
    case "projectAdded":
    case "projectUpdated": {
      const rest = projects.filter((p) => p.id !== event.project.id);
      return [...rest, event.project].sort((a, b) => a.name.localeCompare(b.name));
    }
    case "projectStatusChanged":
      return projects.map((p) => (p.id === event.id ? { ...p, status: event.status } : p));
    case "projectRemoved":
      return projects.filter((p) => p.id !== event.id);
    default:
      return projects;
  }
}
