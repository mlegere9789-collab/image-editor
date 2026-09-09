import { useState } from "react";

import type React from "react";

import DockablePanel, { type PanelPlacement } from "./DockablePanel";

export type PanelGroupMember = { id: string; title: string; content: React.ReactNode };

type Props = {
  members: PanelGroupMember[];
  placement: PanelPlacement;
  onPlacementChange: (id: string, placement: PanelPlacement) => void;
  onDropOnPanel: (draggedId: string, targetId: string) => void;
  onUngroup: (id: string) => void;
};

/** Window > Panel Groups: two or more panels sharing one dockable frame,
 * only one visible at a time, switched by its own tab -- formed by
 * dropping one panel's drag handle onto another's body (see
 * `DockablePanel`'s own `onDropOnPanel`). The frame itself is dragged,
 * docked, floated, and collapsed exactly like a single panel, through
 * the same `DockablePanel`; `members[0]` is the group's own leader, the
 * one whose id the frame's placement is saved under. */
export default function TabbedPanelGroup({ members, placement, onPlacementChange, onDropOnPanel, onUngroup }: Props) {
  const [activeId, setActiveId] = useState(members[0].id);
  const active = members.find((member) => member.id === activeId) ?? members[0];

  return (
    <DockablePanel
      id={members[0].id}
      title={members.map((member) => member.title).join(" / ")}
      placement={placement}
      onPlacementChange={onPlacementChange}
      onDropOnPanel={onDropOnPanel}
    >
      <div className="panel-group__tabs">
        {members.map((member) => (
          <button
            key={member.id}
            type="button"
            className={`panel-group__tab${member.id === activeId ? " panel-group__tab--active" : ""}`}
            onClick={() => setActiveId(member.id)}
          >
            {member.title}
          </button>
        ))}
        <button
          type="button"
          className="panel-group__ungroup"
          title={`Ungroup ${active.title} from this panel`}
          onClick={() => onUngroup(active.id)}
        >
          ⤢
        </button>
      </div>
      {active.content}
    </DockablePanel>
  );
}
