export type SpinnerSize = "sm" | "md" | "lg";

export interface SpinnerProps {
  size?: SpinnerSize;
}

export function Spinner({ size = "md" }: SpinnerProps) {
  return (
    <span
      className={`spinner spinner-${size}`}
      role="status"
      aria-label="Loading"
    />
  );
}
