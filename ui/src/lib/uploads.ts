// How fast each upload goes and how long it has left, from its progress
// as the worker last reported it to serve.

import type { UploadStatus } from "./api-types";

/** One upload's progress at a moment, to measure the next one against. */
export interface Reading {
  sent: number;
  /** When the worker reported it, in UTC nanoseconds. */
  at: number;
}

/** Where an upload is kept among the others: an episode goes to each destination once. */
export const uploadKey = (upload: UploadStatus): string => `${upload.episode}/${upload.destination}`;

/** What is known of how fast an upload goes. */
export interface Measured {
  /** Its last progress reported, to measure the next one against. */
  reading: Reading;
  /** Bytes per second between its last two reports, once there were two. */
  rate: number | null;
}

/**
 * How fast each upload in `uploads` goes, between the progress it was
 * `measured` at before and what it reports now. The times are the
 * worker's own, when it reported, so a page that asks late measures the
 * same; an upload that reported nothing new keeps its rate. A count that
 * went back, as when a failed upload starts again, starts measuring anew.
 */
export function measure(measured: Map<string, Measured>, uploads: UploadStatus[]): Map<string, Measured> {
  const next = new Map<string, Measured>();
  for (const upload of uploads) {
    if (upload.state !== "uploading" || upload.reported_utc_ns == null) {
      continue;
    }
    const key = uploadKey(upload);
    const reading = { sent: upload.bytes_sent, at: upload.reported_utc_ns };
    const before = measured.get(key);
    if (before === undefined || reading.sent < before.reading.sent) {
      next.set(key, { reading, rate: null });
    } else if (reading.at > before.reading.at) {
      const seconds = (reading.at - before.reading.at) / 1e9;
      next.set(key, { reading, rate: (reading.sent - before.reading.sent) / seconds });
    } else {
      next.set(key, before);
    }
  }
  return next;
}

/** Seconds until `upload` is done at `rate` bytes per second, if it moves at all. */
export function timeLeft(upload: UploadStatus, rate: number | null): number | null {
  return rate !== null && rate > 0 ? Math.max(0, upload.bytes_total - upload.bytes_sent) / rate : null;
}
