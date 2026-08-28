import { Component, type ErrorInfo, type ReactNode } from "react";

interface ErrorBoundaryProps {
  children: ReactNode;
}

interface ErrorBoundaryState {
  error: Error | null;
}

/**
 * Last-resort boundary around the whole app. Without it a render error
 * unmounts the React root and leaves a blank page.
 */
export class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("Unhandled UI error", error, info.componentStack);
  }

  render(): ReactNode {
    const { error } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="crash-screen" role="alert">
        <div className="crash-screen-panel">
          <h1 className="crash-screen-title">Something went wrong</h1>
          <p className="crash-screen-message">{error.message}</p>
          <button
            type="button"
            className="btn-secondary crash-screen-reload"
            onClick={() => globalThis.location.reload()}
          >
            Reload
          </button>
        </div>
      </div>
    );
  }
}
