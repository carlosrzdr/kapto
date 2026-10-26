/**
 * The questions the live view asks the bridge for stretches of its last
 * minutes, one at a time: a question asked while one is out waits, the
 * newest replacing any other waiting, and is asked once that is answered.
 *
 * What is looked at can change while a question is out: Back to now, or
 * another span. `forget` says so, and an answer to a question asked before
 * it is then dropped, as is the question waiting, so a late answer never
 * puts back what was left.
 */
export class Questions {
  #out = false;
  #waiting: { question: () => Promise<unknown>; answered: (answer: unknown) => void } | null = null;
  /** Counts what was looked at: an answer is shown only if nothing was forgotten since it was asked. */
  #looking = 0;

  /** Whether a question is out or waiting. */
  get busy(): boolean {
    return this.#out || this.#waiting !== null;
  }

  /**
   * Asks `question` now, or once the one out is answered, giving its
   * answer to `answered` unless what it was asked for has been forgotten
   * by then. A question that fails is let go: the next may be answered.
   */
  ask<T>(question: () => Promise<T>, answered: (answer: T) => void): void {
    this.#waiting = { question, answered: answered as (answer: unknown) => void };
    if (!this.#out) {
      this.#next();
    }
  }

  /** What was asked so far no longer counts: its answers, and the question waiting, are dropped. */
  forget(): void {
    this.#looking += 1;
    this.#waiting = null;
  }

  #next(): void {
    const asked = this.#waiting;
    if (!asked) {
      return;
    }
    this.#waiting = null;
    this.#out = true;
    const looking = this.#looking;
    let answer: Promise<unknown>;
    try {
      answer = asked.question();
    } catch (error) {
      answer = Promise.reject(error);
    }
    answer
      .then((answered) => {
        if (looking === this.#looking) {
          asked.answered(answered);
        }
      })
      .catch(() => {
        // Shown as it was; the next question may be answered.
      })
      .finally(() => {
        this.#out = false;
        this.#next();
      });
  }
}
