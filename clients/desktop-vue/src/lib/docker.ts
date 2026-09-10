// What to tell someone whose Docker is not answering.
//
// The engine classifies the failure (mast-contract's DockerUnavailable); this
// turns the classification into a sentence and a repair. Kept out of the
// component so the wording can be tested — the whole point of the exercise is
// that the words are right, and words are exactly what a rendered card makes
// awkward to assert.
//
// What this replaces: the card used to print the engine's error verbatim, so
// a stopped daemon read "Docker unavailable: docker API error: Error in the
// hyper legacy client: client error (Connect)". Every word of that is true. It
// names an HTTP client library, and the reader's actual problem is that
// dockerd is not running.

import type { DockerUnavailable } from "../bindings";

export interface DockerAdvice {
  /** What is wrong, in the reader's terms. */
  title: string;
  /** What to do about it, in prose. */
  fix: string;
  /** The command that does it, when one command does. Carried separately so
   * the card can render it as code — backticks in a plain-text node render as
   * backticks, which is how markdown syntax ends up on screen. Absent where
   * the answer genuinely is not one command, rather than faked with the most
   * popular platform's. */
  command?: string;
}

const ADVICE: Record<DockerUnavailable, DockerAdvice> = {
  notInstalled: {
    title: "Docker isn't installed",
    // No command: the answer is a different package manager on every system,
    // and a wrong one sends the reader somewhere that does not exist.
    fix: "Install Docker Engine or Docker Desktop. Mast keeps checking and will pick it up on its own once the install finishes — no restart needed.",
  },
  notRunning: {
    title: "Docker is installed but not running",
    fix: "Start the Docker service and Mast reconnects by itself. On macOS and Windows, launch Docker Desktop instead.",
    command: "sudo systemctl start docker",
  },
  permissionDenied: {
    title: "Docker is running but refused the connection",
    fix: "Your user is not permitted to use the Docker socket. After this, log out and back in — group membership only applies to new sessions.",
    command: "sudo usermod -aG docker $USER",
  },
  unreachable: {
    title: "Docker isn't reachable",
    fix: "The endpoint Mast resolved did not answer. Check that the current Docker context points somewhere live.",
    command: "docker context ls",
  },
};

/** Advice for a classified failure. An unclassified one — an older engine, or
 * a client newer than the engine it is talking to — gets the generic case
 * rather than nothing at all. */
export function dockerAdvice(reason: DockerUnavailable | null): DockerAdvice {
  return (reason && ADVICE[reason]) || ADVICE.unreachable;
}
