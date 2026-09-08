const AVATAR_SIZE = 256;

export interface CropArea {
  x: number;
  y: number;
  width: number;
  height: number;
}

function encodeCanvas(canvas: HTMLCanvasElement): Promise<Blob | null> {
  return new Promise<Blob | null>((resolve) => {
    canvas.toBlob((result) => resolve(result), "image/webp", 0.85);
  }).then((blob) => {
    if (blob && blob.type === "image/webp") {
      return blob;
    }
    return new Promise<Blob | null>((resolve) => {
      canvas.toBlob((result) => resolve(result), "image/png");
    });
  });
}

/**
 * Resizes a user-provided image into a square 256x256 avatar (center-crop),
 * re-encoded as WebP to keep uploads small. Falls back to PNG when the
 * browser cannot encode WebP.
 */
export async function resizeImageToAvatar(file: Blob): Promise<Blob> {
  const bitmap = await createImageBitmap(file);
  try {
    const scale = Math.max(AVATAR_SIZE / bitmap.width, AVATAR_SIZE / bitmap.height);
    const drawWidth = Math.round(bitmap.width * scale);
    const drawHeight = Math.round(bitmap.height * scale);
    const offsetX = Math.round((drawWidth - AVATAR_SIZE) / 2);
    const offsetY = Math.round((drawHeight - AVATAR_SIZE) / 2);

    const canvas = document.createElement("canvas");
    canvas.width = AVATAR_SIZE;
    canvas.height = AVATAR_SIZE;
    const context = canvas.getContext("2d");
    if (!context) {
      return file;
    }
    context.drawImage(bitmap, -offsetX, -offsetY, drawWidth, drawHeight);
    return (await encodeCanvas(canvas)) ?? file;
  } finally {
    bitmap.close();
  }
}

/**
 * Crops a user-selected region (natural-pixel coordinates, as reported by the
 * cropper) into a square 256x256 avatar.
 */
export async function cropImageToAvatar(file: Blob, area: CropArea): Promise<Blob> {
  const bitmap = await createImageBitmap(file);
  try {
    const canvas = document.createElement("canvas");
    canvas.width = AVATAR_SIZE;
    canvas.height = AVATAR_SIZE;
    const context = canvas.getContext("2d");
    if (!context) {
      return file;
    }
    context.drawImage(
      bitmap,
      area.x,
      area.y,
      area.width,
      area.height,
      0,
      0,
      AVATAR_SIZE,
      AVATAR_SIZE,
    );
    return (await encodeCanvas(canvas)) ?? file;
  } finally {
    bitmap.close();
  }
}
