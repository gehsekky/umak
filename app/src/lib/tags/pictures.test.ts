import { describe, expect, it } from "vitest";
import type { FilePicture } from "../api/generated/FilePicture";
import { pictureFacts } from "./pictures";

function picture(changes: Partial<FilePicture>): FilePicture {
  return {
    kind: "Front cover",
    description: null,
    mimeType: "image/jpeg",
    bytes: 87_000,
    width: 600,
    height: 600,
    art: "a",
    ...changes,
  };
}

describe("picture facts", () => {
  it("gives the size, format and file size", () => {
    expect(pictureFacts(picture({}))).toEqual(["600 × 600", "JPEG", "85 KB"]);
    expect(pictureFacts(picture({ mimeType: "image/webp" }))).toEqual([
      "600 × 600",
      "image/webp",
      "85 KB",
    ]);
    expect(pictureFacts(picture({ mimeType: null }))).toEqual(["600 × 600", "85 KB"]);
  });

  it("says when a picture cannot be read", () => {
    expect(pictureFacts(picture({ width: null, height: null, art: null }))).toEqual([
      "Cannot be read",
      "JPEG",
      "85 KB",
    ]);
  });
});
