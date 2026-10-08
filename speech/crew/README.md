# Global crew speech

Edit `copilot.toml`, `flight_attendant.toml` or `ground_services.toml` here. These are global files: no IFR/VFR or regional copies are needed. Flight-phase and situation tags still apply.

The copilot speaks in the cockpit, the attendant uses the interphone or passenger PA, and ground services use the ramp interphone. None grants ATC clearance. Aircraft control mappings, checklist order and expected values come from the aircraft profile. All spoken requests, challenges and responses stay in speech TOML. Examples must not pretend a requested action has already completed.

See [the editing guide](../README.md) for fields, placeholders, validation and personal overrides.

ATC station reception does not govern these cockpit/interphone roles. Crew examples omit `service`; Ground ATC is distinct from ground-service crew.

`controls_and_checklists.toml` adds 168 examples for pilot requests, captain checklist responses and crew confirmations. Completed-action examples require a verified live reading. See [control mappings and checklist modes](../../docs/AIRCRAFT_CONTROLS.md).

`cabin_conversation.toml` adds 282 friendly examples in 47 situations covering all 11 flight phases. Tags distinguish `interphone` replies from `passenger_pa` announcements. A greeting does not request cabin preparation. Passenger announcements require an explicit request, and operational status requires confirmed live facts. Refreshment examples ask preferences without claiming catering or delivery is complete.
