import { useEffect, useMemo, useState } from "react";
import Cropper from "react-easy-crop";

import { Button } from "../../shared/ui/Button";
import { Modal } from "../../shared/ui/Modal";
import {
  cropImageToAvatar,
  resizeImageToAvatar,
  type CropArea,
} from "../../shared/lib/image";

export interface AvatarCropModalProps {
  file: Blob;
  onCancel: () => void;
  onApply: (blob: Blob) => void | Promise<void>;
}

const MIN_ZOOM = 1;
const MAX_ZOOM = 10;

/**
 * Lets the user pick a square region of the uploaded image (drag to move,
 * slider to zoom) before it becomes the avatar. Falls back to a center crop
 * when the crop area has not been reported yet. Outside clicks do not dismiss
 * the editor; Cancel / Escape are the explicit close paths.
 */
export function AvatarCropModal({ file, onCancel, onApply }: AvatarCropModalProps) {
  const [crop, setCrop] = useState({ x: 0, y: 0 });
  const [zoom, setZoom] = useState(MIN_ZOOM);
  const [area, setArea] = useState<CropArea | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const imageUrl = useMemo(() => URL.createObjectURL(file), [file]);
  useEffect(() => () => URL.revokeObjectURL(imageUrl), [imageUrl]);

  const handleApply = async () => {
    setBusy(true);
    setError(null);
    try {
      const blob = area
        ? await cropImageToAvatar(file, area)
        : await resizeImageToAvatar(file);
      await onApply(blob);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open
      onClose={onCancel}
      labelledBy="avatar-crop-title"
      closeOnBackdrop={false}
    >
      <div className="avatar-crop-body">
        <h2 id="avatar-crop-title" className="avatar-crop-title">
          Trim icon
        </h2>
        <div className="avatar-crop-area">
          <Cropper
            image={imageUrl}
            crop={crop}
            zoom={zoom}
            minZoom={MIN_ZOOM}
            maxZoom={MAX_ZOOM}
            aspect={1}
            cropShape="round"
            showGrid={false}
            onCropChange={setCrop}
            onZoomChange={setZoom}
            onCropComplete={(_, pixels) => setArea(pixels)}
          />
        </div>
        <label className="avatar-crop-zoom">
          Zoom
          <input
            type="range"
            min={MIN_ZOOM}
            max={MAX_ZOOM}
            step={0.01}
            value={zoom}
            onChange={(event) => setZoom(Number(event.target.value))}
          />
        </label>
        {error && <p className="avatar-crop-error">{error}</p>}
        <div className="avatar-crop-actions">
          <Button variant="secondary" onClick={onCancel}>
            Cancel
          </Button>
          <Button variant="primary" onClick={handleApply} busy={busy}>
            Apply
          </Button>
        </div>
      </div>
    </Modal>
  );
}
