import type { ReactNode } from "react";

import { Spinner } from "../../shared/ui/Spinner";

interface QueryStateProps<T> {
  state: { data: T | undefined; loading: boolean; error: Error | null };
  children: (data: T) => ReactNode;
}

/** Renders the loading / error states of a server query, then its data. */
export function QueryState<T>({ state, children }: QueryStateProps<T>) {
  if (state.data !== undefined) return <>{children(state.data)}</>;
  if (state.error) {
    return (
      <div className="run-error" role="alert">
        {state.error.message}
      </div>
    );
  }
  return (
    <div className="agent-loading">
      <Spinner />
    </div>
  );
}
