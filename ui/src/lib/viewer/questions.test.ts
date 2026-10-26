import { describe, expect, it } from "vitest";

import { Questions } from "./questions";

/** A question whose answer comes when the test says. */
function pending() {
  const asked: { resolve: (answer: string) => void; reject: (error: Error) => void }[] = [];
  const question = () =>
    new Promise<string>((resolve, reject) => {
      asked.push({ resolve, reject });
    });
  return { asked, question };
}

/** Lets every answer already given be handled. */
const settled = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("Questions", () => {
  it("asks one at a time, the newest waiting asked once the one out is answered", async () => {
    const bridge = pending();
    const shown: string[] = [];
    const questions = new Questions();
    questions.ask(bridge.question, (answer) => shown.push(answer));
    questions.ask(bridge.question, (answer) => shown.push(`second ${answer}`));
    questions.ask(bridge.question, (answer) => shown.push(`third ${answer}`));
    expect(bridge.asked).toHaveLength(1);
    expect(questions.busy).toBe(true);
    bridge.asked[0]?.resolve("a");
    await settled();
    expect(bridge.asked).toHaveLength(2);
    bridge.asked[1]?.resolve("b");
    await settled();
    expect(shown).toEqual(["a", "third b"]);
    expect(questions.busy).toBe(false);
  });

  it("drops a late answer to a question asked before going back to now, and the question waiting", async () => {
    const bridge = pending();
    let lookedBack = false;
    const questions = new Questions();
    // Paused, then playing: a question out and one waiting.
    questions.ask(bridge.question, () => (lookedBack = true));
    questions.ask(bridge.question, () => (lookedBack = true));
    // Back to now before the bridge answers.
    questions.forget();
    lookedBack = false;
    bridge.asked[0]?.resolve("late");
    await settled();
    expect(lookedBack).toBe(false);
    expect(bridge.asked).toHaveLength(1);
    expect(questions.busy).toBe(false);
  });

  it("shows the span chosen while a question for the one before is out, never the one before", async () => {
    const bridge = pending();
    const shown: string[] = [];
    const questions = new Questions();
    questions.ask(bridge.question, (answer) => shown.push(answer));
    // Another span chosen: what was asked for no longer counts.
    questions.forget();
    questions.ask(bridge.question, (answer) => shown.push(answer));
    bridge.asked[0]?.resolve("one minute");
    await settled();
    expect(bridge.asked).toHaveLength(2);
    bridge.asked[1]?.resolve("two minutes");
    await settled();
    expect(shown).toEqual(["two minutes"]);
  });

  it("asks the next once a question fails or throws", async () => {
    const bridge = pending();
    const shown: string[] = [];
    const questions = new Questions();
    questions.ask(bridge.question, (answer) => shown.push(answer));
    questions.ask(bridge.question, (answer) => shown.push(answer));
    bridge.asked[0]?.reject(new Error("the bridge didn't answer"));
    await settled();
    bridge.asked[1]?.resolve("answered");
    await settled();
    questions.ask<string>(
      () => {
        throw new Error("not watching the live stream");
      },
      (answer) => shown.push(answer),
    );
    await settled();
    expect(questions.busy).toBe(false);
    expect(shown).toEqual(["answered"]);
  });
});
