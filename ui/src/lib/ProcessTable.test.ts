import { render, screen } from "@testing-library/svelte";
import { describe, expect, it } from "vitest";

import type { ProcessStatus } from "./api-types";
import ProcessTable from "./ProcessTable.svelte";

const capture: ProcessStatus = {
  name: "capture",
  state: "running",
  pid: 4242,
  uptime_s: 75,
  cpu_cores: 0.26,
  memory_bytes: 87_203_840,
  threads: 9,
  restarts: 0,
};

describe("ProcessTable", () => {
  it("shows each running process's uptime, CPU, memory and threads", () => {
    render(ProcessTable, { processes: [capture] });
    const row = screen.getByRole("row", { name: /capture/ });
    expect(row.textContent).toContain("up 1:15");
    expect(row.textContent).toContain("0.26 cores");
    expect(row.textContent).toContain("83 MiB");
    expect(row.textContent).toContain("9");
  });

  it("marks a process waiting to be started again, and says how it ended", () => {
    const ended: ProcessStatus = {
      name: "capture",
      state: "restarting",
      restart_in_s: 4,
      restarts: 2,
      last_exit: "signal: 9 (SIGKILL)",
    };
    render(ProcessTable, { processes: [ended] });
    const cells = screen.getByRole("row", { name: /^capture restarting/ }).querySelectorAll("td");
    expect(cells[0]?.textContent?.trim()).toBe("restarting in 0:04");
    expect(cells[0]?.classList.contains("problem")).toBe(true);
    expect(cells[1]?.textContent).toBe("–");
    expect(cells[4]?.classList.contains("problem")).toBe(true);
    expect(screen.getByText("capture ended: signal: 9 (SIGKILL)")).toBeTruthy();
  });
});
