// Client-side mirror of the engine's layered topo check, so the workspace
// dialog can refuse cyclic dependency selections before they're saved.

export interface GraphMember {
  id: string;
  dependsOn: string[];
}

/** Returns the ids involved in a dependency cycle, or null when acyclic. */
export function findCycle(members: GraphMember[]): string[] | null {
  const ids = new Set(members.map((m) => m.id));
  const remaining = new Map(
    members.map((m) => [m.id, new Set(m.dependsOn.filter((d) => ids.has(d) && d !== m.id))]),
  );
  while (remaining.size > 0) {
    const ready = [...remaining.entries()].filter(([, deps]) => deps.size === 0).map(([id]) => id);
    if (ready.length === 0) return [...remaining.keys()];
    for (const id of ready) remaining.delete(id);
    for (const deps of remaining.values()) for (const id of ready) deps.delete(id);
  }
  return null;
}
