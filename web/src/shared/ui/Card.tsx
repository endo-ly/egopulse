import type { ReactNode, KeyboardEvent } from "react";

export interface CardProps {
  active?: boolean;
  onClick?: () => void;
  children: ReactNode;
}

export function Card({ active = false, onClick, children }: CardProps) {
  const classes = ["card"];
  if (active) classes.push("card-active");
  if (onClick) classes.push("card-clickable");

  const handleKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!onClick) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onClick();
    }
  };

  return (
    <div
      className={classes.join(" ")}
      onClick={onClick}
      onKeyDown={onClick ? handleKeyDown : undefined}
      role={onClick ? "button" : undefined}
      tabIndex={onClick ? 0 : undefined}
    >
      {children}
    </div>
  );
}
