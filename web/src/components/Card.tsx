import type { ReactNode } from "react";

export interface CardProps {
  active?: boolean;
  onClick?: () => void;
  children: ReactNode;
}

export function Card({ active = false, onClick, children }: CardProps) {
  const classes = ["card"];
  if (active) classes.push("card-active");
  if (onClick) classes.push("card-clickable");

  return (
    <div
      className={classes.join(" ")}
      onClick={onClick}
      role={onClick ? "button" : undefined}
      tabIndex={onClick ? 0 : undefined}
    >
      {children}
    </div>
  );
}
