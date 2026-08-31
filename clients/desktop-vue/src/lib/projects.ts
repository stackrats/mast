// Pure patch-application over the project list — the store's reducer for
// project-level events. Discovery/docker/watched-directory events are applied
// directly in the store.

import type { PatchEvent, ProjectSummary } from "../bindings";

/** The one ordering every list shows: the user's dragged rank, name as the
 * tiebreak — so a fleet that was never dragged stays exactly alphabetical. */
export function sortProjects(projects: ProjectSummary[]): ProjectSummary[] {
  return [...projects].sort(
    (a, b) => (a.rank ?? 0) - (b.rank ?? 0) || a.name.localeCompare(b.name),
  );
}

export function applyPatchEvent(projects: ProjectSummary[], event: PatchEvent): ProjectSummary[] {
  switch (event.type) {
    case "projectAdded":
    case "projectUpdated": {
      const rest = projects.filter((p) => p.id !== event.project.id);
      return sortProjects([...rest, event.project]);
    }
    case "projectStatusChanged":
      return projects.map((p) => (p.id === event.id ? { ...p, status: event.status } : p));
    case "projectRemoved":
      return projects.filter((p) => p.id !== event.id);
    default:
      return projects;
  }
}
