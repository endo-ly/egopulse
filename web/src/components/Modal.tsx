import type { ReactNode } from "react";

type ModalProps = {
  children: ReactNode;
  onClose: () => void;
  labelledBy: string;
};

export function Modal({ children, onClose, labelledBy }: ModalProps) {
  return (
    <div
      className="fixed inset-0 grid place-items-center bg-[rgba(3,5,12,0.75)] p-5"
      onClick={onClose}
      onKeyDown={(event) => {
        if (event.key === "Escape") onClose();
      }}
      role="presentation"
    >
      <div
        className="w-full max-w-3xl max-h-[90vh] flex flex-col border border-border rounded-[28px] bg-gradient-to-b from-[rgba(10,16,30,0.98)] to-[rgba(6,10,20,0.98)] shadow-[0_28px_60px_rgba(0,0,0,0.3)]"
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelledBy}
        onClick={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape") onClose();
        }}
      >
        {children}
      </div>
    </div>
  );
}
