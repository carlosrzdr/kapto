// The worker of `pictures-off-thread.ts`: decodes each picture it is asked
// to and hands the bitmap back, moved rather than copied.

import { type DecodeAnswer, type DecodeAsk, decode } from "./pictures-off-thread";

/** This worker's own scope; the page's types describe a window. */
const scope = self as unknown as {
  onmessage: ((message: MessageEvent<DecodeAsk>) => void) | null;
  postMessage: (answer: DecodeAnswer, transfer?: Transferable[]) => void;
};

scope.onmessage = (message) => {
  const { id, ...ask } = message.data;
  decode(ask)
    .then((bitmap) => scope.postMessage({ id, bitmap }, [bitmap]))
    .catch((error: unknown) => scope.postMessage({ id, error: error instanceof Error ? error.message : String(error) }));
};
