import { expect, test } from "vitest";
import { render, screen } from "@testing-library/react";

import StatusBar from "../StatusBar";

test("status_bar_flags_a_stale_model_only_when_the_latest_compile_failed", () => {
  const { rerender } = render(
    <StatusBar stats={null} compiling={false} lastMs={12} errorCount={1} stale={true} />,
  );
  expect(screen.getByTestId("status-stale").textContent).toBe("showing last good model");
  rerender(<StatusBar stats={null} compiling={false} lastMs={12} errorCount={0} stale={false} />);
  expect(screen.queryByTestId("status-stale")).toBeNull();
});
