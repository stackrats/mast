import { describe, expect, it } from "vite-plus/test";

import { formatElapsed } from "./elapsed";

describe("formatElapsed", () => {
  it("counts seconds under a minute", () => {
    expect(formatElapsed(0)).toBe("0s");
    expect(formatElapsed(9_400)).toBe("9s");
    expect(formatElapsed(59_999)).toBe("59s");
  });

  it("pads seconds inside a minute so the readout stops jittering in width", () => {
    expect(formatElapsed(60_000)).toBe("1m 00s");
    expect(formatElapsed(252_000)).toBe("4m 12s");
  });

  // A cold Sail runtime build is the case this exists for, and seconds stop
  // carrying information long before it finishes.
  it("drops seconds past an hour", () => {
    expect(formatElapsed(3_960_000)).toBe("1h 06m");
    expect(formatElapsed(7_200_000)).toBe("2h 00m");
  });

  it("never renders a negative age from a clock that stepped backwards", () => {
    expect(formatElapsed(-5_000)).toBe("0s");
  });
});
