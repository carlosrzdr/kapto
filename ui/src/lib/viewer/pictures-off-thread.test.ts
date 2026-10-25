import { describe, expect, it } from "vitest";

import { type DecodeAnswer, type DecodeAsk, OffThreadDecoder } from "./pictures-off-thread";

/** A worker that keeps what it is asked and answers when told to. */
class FakeWorker {
  onmessage: ((message: MessageEvent<DecodeAnswer>) => void) | null = null;
  asked: { ask: DecodeAsk; transfer: Transferable[] }[] = [];
  stopped = false;
  postMessage(ask: DecodeAsk, transfer: Transferable[]) {
    this.asked.push({ ask, transfer });
  }
  answer(answer: DecodeAnswer) {
    this.onmessage?.({ data: answer } as MessageEvent<DecodeAnswer>);
  }
  terminate() {
    this.stopped = true;
  }
}

/** A stand-in bitmap that says whether it was let go of. */
function bitmap(width: number) {
  const made = { width, closed: false, close: () => (made.closed = true) };
  return made as unknown as ImageBitmap & { closed: boolean };
}

describe("OffThreadDecoder", () => {
  it("hands each picture's bytes to the worker and each bitmap back to whoever asked", async () => {
    const worker = new FakeWorker();
    const decoder = new OffThreadDecoder(() => worker as unknown as Worker);
    const message = new ArrayBuffer(64);
    const first = decoder.decode(new Uint8Array(message, 40, 24), "image/jpeg", 320);
    const second = decoder.decode(new Uint8Array(new ArrayBuffer(8)), "image/png", null);

    expect(worker.asked.map(({ ask }) => [ask.id, ask.type, ask.resizeWidth])).toEqual([
      [0, "image/jpeg", 320],
      [1, "image/png", null],
    ]);
    // The message's buffer moves to the worker rather than being copied.
    expect(worker.asked[0]?.transfer).toEqual([message]);

    // Answered out of order, each still reaches its own question.
    worker.answer({ id: 1, bitmap: bitmap(2) });
    worker.answer({ id: 0, bitmap: bitmap(320) });
    expect((await first).width).toBe(320);
    expect((await second).width).toBe(2);
  });

  it("says why a picture couldn't be decoded, and lets go of a bitmap nobody waits for", async () => {
    const worker = new FakeWorker();
    const decoder = new OffThreadDecoder(() => worker as unknown as Worker);
    const failing = decoder.decode(new Uint8Array(new ArrayBuffer(4)), "image/jpeg", null);
    worker.answer({ id: 0, error: "The source image could not be decoded." });
    await expect(failing).rejects.toThrow("could not be decoded");

    const unasked = bitmap(10);
    worker.answer({ id: 7, bitmap: unasked });
    expect(unasked.closed).toBe(true);

    decoder.stop();
    expect(worker.stopped).toBe(true);
  });
});
