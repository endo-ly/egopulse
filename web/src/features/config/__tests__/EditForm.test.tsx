import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConfigConflictError } from "../../../shared/api/config";
import { EditForm, type FieldSpec, type FormValues } from "../EditForm";

const SPECS: FieldSpec[] = [
  { key: "label", label: "Name", kind: "text" },
  { key: "model", label: "Model", kind: "text", nullable: true, placeholder: () => "provider default" },
];

function renderForm(values: FormValues, onSave = vi.fn().mockResolvedValue(undefined), onReload = vi.fn()) {
  const view = render(
    <EditForm formId="test" specs={SPECS} values={values} onSave={onSave} onCancel={() => {}} onReload={onReload} />,
  );
  return { ...view, onSave, onReload };
}

afterEach(cleanup);

/** Whether leaving the page would now ask for confirmation. */
function unloadIsGuarded(): boolean {
  const event = new Event("beforeunload", { cancelable: true });
  globalThis.dispatchEvent(event);
  return event.defaultPrevented;
}

describe("EditForm", () => {
  it("sends_only_the_changed_fields", async () => {
    // Arrange
    const { onSave } = renderForm({ label: "Lyre", model: null });

    // Act
    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "gpt-5" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    // Assert
    await waitFor(() => expect(onSave).toHaveBeenCalledWith({ model: "gpt-5" }));
    expect(screen.getByText("Not set (provider default)")).toBeTruthy();
  });

  it("keeps_save_disabled_without_changes_and_marks_the_form_dirty_once_edited", () => {
    // Arrange
    renderForm({ label: "Lyre", model: null });
    const save = screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;

    // Act / Assert
    expect(save.disabled).toBe(true);
    expect(unloadIsGuarded()).toBe(false);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Lyra" } });
    expect(save.disabled).toBe(false);
    expect(unloadIsGuarded()).toBe(true);
  });

  it("follows_server_changes_for_untouched_fields_and_flags_concurrent_edits", () => {
    // Arrange
    const { rerender } = renderForm({ label: "Lyre", model: "a" });
    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "mine" } });

    // Act: both fields change elsewhere while the form is open.
    rerender(
      <EditForm
        formId="test"
        specs={SPECS}
        values={{ label: "Lyre 2", model: "theirs" }}
        onSave={vi.fn()}
        onCancel={() => {}}
        onReload={() => {}}
      />,
    );

    // Assert
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("Lyre 2");
    expect((screen.getByLabelText("Model") as HTMLInputElement).value).toBe("mine");
    expect(screen.getByText("1 change")).toBeTruthy();
    expect(screen.getByText("theirs")).toBeTruthy();
    expect(screen.getByText(/Also changed elsewhere/)).toBeTruthy();
  });

  it("reloads_and_keeps_the_draft_on_conflict", async () => {
    // Arrange
    const onSave = vi.fn().mockRejectedValue(new ConfigConflictError());
    const { onReload } = renderForm({ label: "Lyre", model: null }, onSave);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Lyra" } });

    // Act
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    // Assert
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("changed elsewhere"));
    expect(onReload).toHaveBeenCalledTimes(1);
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("Lyra");
  });

  it("rejects_an_empty_required_field", () => {
    // Arrange
    renderForm({ label: "Lyre", model: null });

    // Act
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "" } });

    // Assert
    expect(screen.getByText("Required")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(true);
  });
});
