import type { ButtonHTMLAttributes, ReactNode } from "react";

export type ButtonVariant = "primary" | "secondary" | "icon" | "danger";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant: ButtonVariant;
  busy?: boolean;
  children: ReactNode;
}

export function Button({
  variant,
  busy = false,
  disabled,
  children,
  className,
  ...rest
}: ButtonProps) {
  const classes = [`btn-${variant}`];
  if (className) classes.push(className);

  return (
    <button
      className={classes.join(" ")}
      disabled={disabled || busy}
      aria-busy={busy || undefined}
      {...rest}
    >
      {busy && <span className="btn-spinner" aria-hidden="true" />}
      {children}
    </button>
  );
}
