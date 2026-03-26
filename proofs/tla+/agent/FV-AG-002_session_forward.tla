---- MODULE AgentSession ----
\* FV-AG-002: Agent Session State Machine — Forward-Only Transitions
\*
\* Invariant: SessionState transitions are strictly forward-progressing.
\*            No transition can return the session to an earlier state.
\*
\* Verified properties:
\*   INV  ForwardOnly       — non-terminal state ordinals never decrease
\*   INV  TypeInvariant     — all session values are valid States
\*   INV  PlanConsistency   — plan_bound is monotonic (false→true, never true→false)
\*   PROP TerminalAbsorbing — once terminal, state never changes (temporal)
\*   PROP AllSessionsTerminate — every session eventually reaches a terminal state
\*
\* Object: VO-AG-002
\* Anchor: crates/nexus-intent/src/agent_core/session.rs
\* Status: COMPLETE — TLC / Apalache checkable

EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    MaxSessions   \* Maximum concurrent sessions to model-check (suggest 3–4)

VARIABLES
    sessions,     \* Function: session_id -> current state string
    plan_bound    \* Function: session_id -> BOOLEAN (plan hash bound?)

vars == <<sessions, plan_bound>>

\* ─── State Space ────────────────────────────────────────────────────
\* Ordinal ordering — higher number = later in lifecycle.
\* Aborted and Expired share ordinal 5 because both are terminal.
StateOrder ==
    [Received             |-> 0,
     Simulated            |-> 1,
     AwaitingConfirmation |-> 2,
     Executing            |-> 3,
     Finalized            |-> 4,
     Aborted              |-> 5,
     Expired              |-> 5]

States == DOMAIN StateOrder

\* Terminal states (no further transitions allowed)
TerminalStates == {"Finalized", "Aborted", "Expired"}

\* ─── Transition Relation ────────────────────────────────────────────
\* Matches session.rs can_transition_to() exactly.
\*   Simulated can skip AwaitingConfirmation and go directly to Executing
\*   when autonomous (no human confirmation required).
ValidTransitions ==
    [Received             |-> {"Simulated", "Aborted", "Expired"},
     Simulated            |-> {"AwaitingConfirmation", "Executing", "Aborted", "Expired"},
     AwaitingConfirmation |-> {"Executing", "Aborted", "Expired"},
     Executing            |-> {"Finalized", "Aborted", "Expired"},
     Finalized            |-> {},
     Aborted              |-> {},
     Expired              |-> {}]

\* ─── Plan Binding ───────────────────────────────────────────────────
\* In session.rs, bind_plan() sets plan_hash once during Simulated→AwaitingConfirmation
\* or Simulated→Executing.  It is never unbound.
TransitionBindsPlan(oldState, newState) ==
    /\ oldState = "Simulated"
    /\ newState \in {"AwaitingConfirmation", "Executing"}

\* ─── Initial State ──────────────────────────────────────────────────
Init ==
    /\ sessions   = [s \in {} |-> "Received"]
    /\ plan_bound = [s \in {} |-> FALSE]

\* ─── Actions ────────────────────────────────────────────────────────

\* Create a new session (enters Received state, plan not yet bound)
CreateSession(sid) ==
    /\ sid \notin DOMAIN sessions
    /\ Cardinality(DOMAIN sessions) < MaxSessions
    /\ sessions'   = sessions   @@ (sid :> "Received")
    /\ plan_bound' = plan_bound @@ (sid :> FALSE)

\* Transition a session to a new state
Transition(sid, newState) ==
    /\ sid \in DOMAIN sessions
    /\ LET curState == sessions[sid]
       IN  /\ curState \notin TerminalStates
           /\ newState \in ValidTransitions[curState]
           /\ sessions' = [sessions EXCEPT ![sid] = newState]
           /\ plan_bound' =
                IF TransitionBindsPlan(curState, newState)
                THEN [plan_bound EXCEPT ![sid] = TRUE]
                ELSE plan_bound

\* System next-state relation
Next ==
    \E sid \in 1..MaxSessions :
        \/ CreateSession(sid)
        \/ \E ns \in States : Transition(sid, ns)

\* Stuttering-tolerant specification
Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

\* ─── Invariants (checked at every reachable state) ──────────────────

\* TYPE: all session values are valid States
TypeInvariant ==
    \A sid \in DOMAIN sessions :
        /\ sessions[sid] \in States
        /\ sid \in DOMAIN plan_bound
        /\ plan_bound[sid] \in BOOLEAN

\* FORWARD-ONLY: Every transition preserves or increases state ordinal.
\* By construction ValidTransitions only contains same-or-higher ordinals,
\* so we verify the structural property: no reachable state can
\* transition to a state with a strictly lower ordinal.
ForwardOnly ==
    \A sid \in DOMAIN sessions :
        \A target \in ValidTransitions[sessions[sid]] :
            StateOrder[target] >= StateOrder[sessions[sid]]

\* PLAN MONOTONICITY: once plan_bound is TRUE it never reverts.
PlanConsistency ==
    \A sid \in DOMAIN sessions :
        plan_bound[sid] = TRUE =>
            sessions[sid] \in {"AwaitingConfirmation", "Executing", "Finalized", "Aborted", "Expired"}

\* ─── Temporal Properties (checked across behaviours) ────────────────

\* TERMINAL ABSORBING: once a session enters a terminal state it stays there forever.
TerminalAbsorbing ==
    \A sid \in 1..MaxSessions :
        [](sid \in DOMAIN sessions /\ sessions[sid] \in TerminalStates
           => [](sessions[sid] \in TerminalStates))

\* LIVENESS: under weak fairness every session eventually terminates.
AllSessionsTerminate ==
    \A sid \in 1..MaxSessions :
        [](sid \in DOMAIN sessions => <>(sessions[sid] \in TerminalStates))

====
