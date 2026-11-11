// How the tag editor describes the pictures in a file.

import type { FilePicture } from "../api/generated/FilePicture";
import { formatSize } from "../lists/format";

const FORMATS: Record<string, string> = {
  "image/jpeg": "JPEG",
  "image/png": "PNG",
  "image/gif": "GIF",
  "image/bmp": "BMP",
  "image/tiff": "TIFF",
};

/** Such as ["600 × 600", "JPEG", "85 KB"], the first saying if the picture cannot be read. */
export function pictureFacts(picture: FilePicture): string[] {
  const format = picture.mimeType ? (FORMATS[picture.mimeType] ?? picture.mimeType) : null;
  const size =
    picture.width !== null && picture.height !== null
      ? `${picture.width} × ${picture.height}`
      : "Cannot be read";
  return [size, format, formatSize(picture.bytes)].filter((part) => part !== null);
}
