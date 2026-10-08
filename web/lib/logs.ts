import type { LogChunk } from "./types";

export type LogLine = {
  key: string;
  ts: string;
  service: string;
  text: string;
  error: boolean;
};

const ERROR_WORDS = /\b(error|panic|fatal|exception|traceback|failed)\b/i;
// Stack frames and indented follow-up lines belong to the error above them.
const CONTINUATION = /^\s+(at |File |from |\.\.\.)|^\s{4,}\S/;

/** Splits stored chunks into lines and marks error lines (and their stack frames). */
export function toLines(chunks: LogChunk[]): LogLine[] {
  const inError = new Map<string, boolean>();
  const lines: LogLine[] = [];
  for (const chunk of chunks) {
    const parts = chunk.text.split("\n");
    if (parts[parts.length - 1] === "") parts.pop();
    parts.forEach((text, i) => {
      const clean = text.replace(/\r$/, "");
      const carries = inError.get(chunk.service_id) === true && CONTINUATION.test(clean);
      const error = carries || ERROR_WORDS.test(clean);
      inError.set(chunk.service_id, error);
      lines.push({ key: `${chunk.id}:${i}`, ts: chunk.ts, service: chunk.service, text: clean, error });
    });
  }
  return lines;
}

export function formatLogTime(iso: string): string {
  return new Date(iso).toLocaleString(undefined, {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}
