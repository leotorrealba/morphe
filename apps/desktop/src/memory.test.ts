import { expect, test } from "vitest";

import { activeConstraints, type DesignEvent } from "./memory";

test("retired constraint is hidden, re-added one is active again", () => {
  const events: DesignEvent[] = [
    { kind: "constraint", name: "wall", value: "2mm" },
    { kind: "constraint_retired", name: "wall" },
    { kind: "constraint", name: "wall", value: "3mm" },
    { kind: "constraint", name: "hole", value: "M3" },
    { kind: "constraint_retired", name: "hole" },
  ];
  expect(activeConstraints(events)).toEqual([{ kind: "constraint", name: "wall", value: "3mm" }]);
});

test("an updated constraint shows only its latest value", () => {
  const events: DesignEvent[] = [
    { kind: "constraint", name: "a", value: "1" },
    { kind: "constraint", name: "b", value: "x" },
    { kind: "constraint", name: "a", value: "2" },
  ];
  expect(activeConstraints(events)).toEqual([
    { kind: "constraint", name: "b", value: "x" },
    { kind: "constraint", name: "a", value: "2" },
  ]);
});
