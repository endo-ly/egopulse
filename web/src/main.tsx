import { createRoot } from "react-dom/client";
import "./app.css";
import { WebUI } from "./app/WebUI";
import { ErrorBoundary } from "./app/ErrorBoundary";

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <ErrorBoundary>
      <WebUI />
    </ErrorBoundary>,
  );
}
