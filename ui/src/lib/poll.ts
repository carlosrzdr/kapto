/**
 * `task` made safe to run on a timer: a run while the last one is still
 * waiting for its answer is skipped. A slow server then gets one request
 * of each kind at a time, rather than a pile that takes up the browser's
 * few connections to it and holds up everything else the page asks.
 *
 * A run that fails ends the wait like one that succeeds. Saying what
 * failed is the task's own job; here the failure is only taken, so it
 * never surfaces as an uncaught error.
 */
export function oneAtATime(task: () => Promise<void>): () => void {
  let waiting = false;
  const done = () => {
    waiting = false;
  };
  return () => {
    if (waiting) {
      return;
    }
    waiting = true;
    task().then(done, done);
  };
}
