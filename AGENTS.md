AgentPulse — Master Project Blueprint
File name: AGENTPULSE_BLUEPRINT.md
Project: AgentPulse
Project type: Cross-platform AI Usage Monitoring Desktop Application
Initial target: Individual / Pro Users
Platforms: Windows and macOS
Development approach: Performance-first, modular, privacy-focused architecture.
1. Project Overview
What is AgentPulse?
AgentPulse is a desktop application that consolidates AI coding assistant usage, subscription limits, token consumption, context-window usage, and session activity into one unified dashboard.

Instead of opening multiple AI applications to check their individual usage, users can open AgentPulse and see everything in one place.

The primary objective is:

Give users complete visibility into their AI coding assistant usage, remaining subscription limits, context consumption, and reset schedules.

AgentPulse should operate quietly in the background, collecting available usage information without noticeably affecting system performance.
2. Supported AI Providers
The initial application will focus on individual subscriptions.
Provider	Product	Initial Support
Anthropic	Claude Pro / Max	Yes
OpenAI	ChatGPT Plus / Pro (Codex)	Yes
Google	Antigravity	Yes
Google	Gemini CLI	Yes
Cursor	Cursor Pro	Later
Windsurf	Windsurf Pro	Later


Important distinction:

We monitor provider accounts, not individual application installations.

For example, Claude Code and other Anthropic-related usage should be associated with the appropriate Anthropic account rather than treated as separate subscriptions.

Provider integrations must only display information that can actually be collected.

Do not invent unavailable metrics or assume that every provider exposes a public personal-subscription API.
3. Technology Stack
The application must prioritize performance, low resource consumption, responsiveness, and long-term maintainability.
Layer	Technology	Purpose
Desktop Framework	Tauri 2	Native desktop application
Frontend	React	Dashboard interface
Language	TypeScript	Frontend development
Backend	Rust	Monitoring engine and collectors
Async Runtime	Tokio	Concurrent background tasks
Database	SQLite	Local persistent storage
Database Optimization	SQLite WAL	Efficient database reads and writes
UI Styling	Tailwind CSS	Interface styling
Charts	Recharts	Usage visualization
Telemetry	OpenTelemetry	Supported provider telemetry
Communication	Tauri Events	Frontend/backend communication
Package Manager	pnpm	Dependency management
Build Tool	Vite	Frontend development and bundling


Technology decision
We will use:

Tauri 2 + Rust + React + TypeScript + SQLite.

Rust handles the monitoring engine and background processes.

React handles the dashboard and user interface.

Tauri connects the frontend and backend while providing native desktop functionality.

SQLite stores usage information locally.
4. System Architecture
AgentPulse must use a modular architecture.

The monitoring engine must be independent of the frontend.

This means we can add new AI providers without rebuilding the dashboard.
High-level architecture
                  AGENTPULSE
                      |
                TAURI 2 CORE
                      |
           +----------+----------+
           |                     |
        REACT UI              RUST ENGINE
           |                     |
       Dashboard             Tokio Runtime
       Analytics                  |
       Sessions          +--------+--------+
       Accounts          |        |        |
       Settings        Claude   Codex    Google
                       Agent    Agent    Agent
                         \       |       /
                          \      |      /
                       DATA COLLECTORS
                              |
                       DATA NORMALIZER
                              |
                        USAGE ENGINE
                              |
                           SQLITE
                              |
                      TAURI EVENT SYSTEM
                              |
                        REACT DASHBOARD

Core architectural principles
1. Collectors must operate independently.
2. The frontend must never communicate directly with AI providers.
3. Rust handles background collection and processing.
4. All provider information must follow a normalized data structure.
5. SQLite is the primary local data store.
6. Collection must not block dashboard interactions.
7. Collectors must support independent initialization and failure handling.
8. The architecture must support future Teams and Enterprise dashboards.
5. Core Application Modules
AgentPulse will initially have the following modules.
Module	Responsibility
Account Manager	Connect and manage AI provider accounts
Provider Collectors	Retrieve usage information
Session Monitor	Track active AI coding sessions
Context Monitor	Track context-window consumption
Subscription Monitor	Track subscription limits and resets
Usage Engine	Normalize and calculate usage
Analytics Engine	Generate historical usage statistics
Notification Engine	Notify users about usage thresholds
Database Manager	Store and retrieve usage information
Dashboard	Display all collected information
Settings	Manage application preferences


Each module should have a clearly defined responsibility.

Avoid creating a monolithic backend.
6. Provider Collection Architecture
Each AI provider must have its own independent collector.

Collectors must implement a shared interface so that the monitoring engine can communicate with every provider consistently.
Initial collectors
Collector	Technology	Priority
Antigravity Collector	Rust	1
Gemini CLI Collector	Rust + OpenTelemetry	2
Claude Code Collector	Rust + supported telemetry/local data	3
Codex Collector	Rust + supported local interfaces	4
Cursor Collector	Rust + official API	Later
Windsurf Collector	Rust + official API	Later


How collection works
         PROVIDER ACCOUNT
                |
                v
         PROVIDER COLLECTOR
                |
                v
        COLLECT AVAILABLE DATA
                |
                v
         VALIDATE INFORMATION
                |
                v
         NORMALIZE DATA FORMAT
                |
                v
          SAVE TO SQLITE
                |
                v
          UPDATE DASHBOARD

Collection methods
AgentPulse should prioritize:
1. Official provider APIs.
2. Official CLI interfaces.
3. Official telemetry integrations.
4. Documented local session information.
5. Estimates only when explicitly identified as estimates.
Never depend on undocumented private APIs without clearly documenting the risks and limitations.

Do not assume that a CLI command is an HTTP endpoint.

For example, Antigravity's documented usage and context interfaces must be verified against the actual installed version before implementation.
7. Unified Usage Data Structure
One of the most important architectural requirements is data normalization.

Different providers return different information.

AgentPulse must convert that information into a consistent internal format.
Example normalized usage object
{
  "provider": "anthropic",
  "account_id": "account_001",
  "session_id": "session_001",
  "model": "claude-model",
  "usage": {
    "input_tokens": 120000,
    "output_tokens": 45000,
    "cached_tokens": 20000
  },
  "context": {
    "used_tokens": 80000,
    "total_tokens": 200000,
    "percentage": 40
  },
  "subscription": {
    "used_percentage": null,
    "remaining_percentage": null,
    "reset_at": null
  },
  "source": {
    "type": "local_telemetry",
    "verified": true
  },
  "collected_at": "2026-11-03T03:00:00Z"
}

The values above are illustrative, not actual provider measurements.
Source classification
Every metric must have a source classification.
Classification	Meaning
Official API	Retrieved through an official API
Official Telemetry	Retrieved through supported telemetry
Local	Retrieved from documented local interfaces
Estimated	Calculated from available information
Unavailable	Provider does not expose the metric


Never present estimated information as actual usage.

Also, never assume that token consumption and subscription quota consumption are interchangeable.
8. Dashboard Structure
The initial dashboard should contain five main sections.
A. Overview Dashboard
The main landing page.

Displays:
- Total connected accounts.
- Overall AI usage.
- Remaining subscription allowances.
- Active AI sessions.
- Context consumption.
- Upcoming quota resets.
- Usage warnings.
Example:
Provider	Usage	Remaining	Status
Claude	65%	35%	Normal
Codex	42%	58%	Normal
Antigravity	70%	30%	Warning


Illustrative data only.
B. Accounts
Displays connected provider accounts.

Features:
- Connect account.
- Disconnect account.
- Account status.
- Provider information.
- Last successful synchronization.
- Collection status.
- Error handling.
Avoid duplicate account records.
C. Sessions
Displays individual AI coding sessions.
Agent	Model	Context	Status
Claude Code	Claude Model	68%	Working
Codex	GPT Model	42%	Working
Antigravity	Gemini Model	75%	Working
Gemini CLI	Gemini Model	30%	Idle


Features:
- Active sessions.
- Session history.
- Model information.
- Context consumption.
- Input/output tokens.
- Session duration.
- Session activity.
Only show information that the corresponding collector can retrieve.
D. Usage Analytics
Displays historical usage information.

Features:
- Daily usage.
- Weekly usage.
- Monthly usage.
- Token consumption.
- Provider comparison.
- Model usage.
- Usage trends.
E. Settings
Features:
- Application preferences.
- Notification thresholds.
- Connected accounts.
- Data management.
- Startup behavior.
- Background monitoring.
- Theme preferences.
- Privacy settings.
9. Subscription and Context Monitoring
AgentPulse must distinguish between two different usage limitations.
Subscription Usage
Measures how much of the provider's subscription allowance has been consumed.

Examples:
- Claude Pro usage.
- ChatGPT Plus / Pro Codex allowance.
- Antigravity model quota.
Session Context
Measures how much context a particular AI session has consumed.

These are completely different metrics.

Example:
Monitor	Usage	Alert
Claude Subscription	80%	Warning
Claude Session Context	90%	Critical
Codex Subscription	45%	Normal
Antigravity Context	95%	Critical


Notification thresholds
Default notification thresholds:
- 80% — Warning.
- 90% — Critical.
- 100% — Limit reached.
Users should eventually be able to customize these values.

Notifications should also support quota reset reminders.
10. Database Architecture
Use SQLite as the primary local database.

Recommended initial entities:
Table	Responsibility
providers	AI provider definitions
accounts	Connected provider accounts
sessions	AI coding sessions
models	AI model information
usage_records	Token consumption records
context_records	Context consumption records
quotas	Subscription limits
billing_records	API costs
notifications	Notification history
collector_status	Collector health and synchronization
app_settings	Application preferences


Database requirements
- Use SQLite WAL mode.
- Add indexes to frequently queried columns.
- Use database migrations.
- Avoid unnecessary database writes.
- Store timestamps consistently in UTC.
- Use transactions for related updates.
- Prevent duplicate usage records.
- Implement retention policies for historical data.
Credentials and authentication tokens must be stored securely using appropriate operating-system credential storage.

Do not store sensitive credentials as plain text in SQLite.
11. Performance Requirements
Performance is one of AgentPulse's primary requirements.

The application should remain lightweight even when monitoring multiple providers simultaneously.
Initial performance targets
Metric	Target
Application startup	Under 2 seconds
Idle CPU	Ideally below 1%
Idle memory	Target below 100 MB
Dashboard responsiveness	No noticeable freezing
Background monitoring	Non-blocking
Database operations	Asynchronous where appropriate
Collector execution	Independent
UI updates	Efficient and selective


These are engineering targets, not guaranteed benchmarks.
Optimization strategies
1. Use Tokio for asynchronous background tasks.
2. Run collectors independently.
3. Avoid unnecessary polling.
4. Use event-driven collection when supported.
5. Batch database writes where appropriate.
6. Use SQLite WAL.
7. Avoid unnecessary React re-renders.
8. Implement selective state updates.
9. Lazy-load nonessential dashboard components.
10. Avoid retaining unnecessary session history in memory.
Important: Never poll every provider every second just to achieve real-time updates.

Collection intervals should depend on provider capabilities, rate limits, and the importance of the metric.
12. Privacy and Security
AgentPulse should follow a local-first architecture.

The application should not upload private user information unnecessarily.
Privacy requirements
- Store usage information locally.
- Do not collect prompts.
- Do not collect source code.
- Do not collect private conversations.
- Do not upload session contents.
- Protect authentication credentials.
- Use secure credential storage.
- Provide account disconnection.
- Allow users to delete local monitoring data.
The primary purpose is usage monitoring, not conversation monitoring.
13. Future Expansion
Although the initial release focuses on individual subscriptions, the architecture must support future expansion.

We will eventually have three dashboard experiences.
Dashboard	Target Users	Priority
Individual / Pro	Personal AI subscriptions	First
Teams	Development teams	Later
Enterprise	Organizations	Later


Individual / Pro
- Personal accounts.
- Subscription monitoring.
- Session tracking.
- Token consumption.
- Context monitoring.
- Notifications.
Teams
- Multiple team members.
- Shared usage analytics.
- Team account monitoring.
- Usage comparisons.
- Team notifications.
Enterprise
- Organization-wide monitoring.
- Centralized administration.
- Role-based access.
- Organization analytics.
- Advanced reporting.
Do not implement Teams or Enterprise functionality during the initial MVP.

However, avoid architectural decisions that would make future expansion unnecessarily difficult.
14. Development Roadmap
Development should follow a controlled, incremental approach.
Phase	Task	Priority
1	Project initialization	Critical
2	Tauri 2 + React + TypeScript setup	Critical
3	Rust backend foundation	Critical
4	SQLite integration	Critical
5	Database architecture and migrations	High
6	Collector architecture	High
7	Antigravity Collector	High
8	Gemini CLI Collector	High
9	Claude Code Collector	High
10	Codex Collector	High
11	Unified Usage Engine	High
12	Account Management	High
13	Dashboard implementation	High
14	Session Monitoring	High
15	Usage Analytics	High
16	Notifications	High
17	Performance optimization	High
18	Windows testing	High
19	macOS testing	High
20	Production packaging	High


First MVP milestone
The first working milestone should accomplish:
1. Successfully launch AgentPulse as a desktop application.
2. Connect the React frontend to the Rust backend.
3. Initialize SQLite.
4. Establish the collector architecture.
5. Implement the first Antigravity integration.
6. Collect actual available usage information.
7. Normalize the collected data.
8. Save the information locally.
9. Display the collected information in the dashboard.
Do not build the entire dashboard before verifying that the monitoring engine can retrieve real data.
15. Engineering Rules
All developers and AI coding agents must follow these rules.
Architecture
- Maintain separation between frontend and backend.
- Keep provider collectors independent.
- Use a shared collector interface.
- Follow consistent naming conventions.
- Avoid unnecessary dependencies.
- Avoid monolithic components and modules.
Code Quality
- Use TypeScript strict mode.
- Follow Rust best practices.
- Handle errors properly.
- Implement appropriate logging.
- Add tests for critical functionality.
- Avoid unnecessary abstractions.
- Keep modules maintainable.
Performance
- Prioritize low resource consumption.
- Avoid unnecessary polling.
- Avoid blocking operations.
- Optimize database access.
- Prevent excessive UI re-rendering.
Data Accuracy
- Never fabricate usage metrics.
- Never assume unavailable provider APIs exist.
- Clearly distinguish official, local, estimated, and unavailable data.
- Verify provider documentation before implementing integrations.
- Preserve original collection timestamps.
Development Workflow
- Work incrementally.
- Explain architectural decisions.
- Do not introduce unnecessary technologies.
- Do not rewrite working modules without justification.
- Test each module before proceeding.
- Keep documentation updated.
16. Instructions for Claude Code
The following instructions should be given to Claude Code after placing this document in the repository root.
You are working on AgentPulse.

Before making any code changes, read the entire
AGENTPULSE_BLUEPRINT.md file.

This document is the primary source of truth for
the project's goals, architecture, technology stack,
performance requirements, and development roadmap.

YOUR FIRST TASK:

Analyze the requirements and propose a complete
project architecture.

Do not immediately start implementing the application.

First, provide:

1. Recommended project directory structure.
2. Frontend architecture.
3. Rust backend architecture.
4. Tauri integration architecture.
5. Database schema proposal.
6. Provider collector architecture.
7. Communication between React and Rust.
8. Background monitoring strategy.
9. Performance optimization strategy.
10. Security and credential management strategy.
11. Development milestones.

TECHNOLOGY REQUIREMENTS:

- Tauri 2
- Rust
- React
- TypeScript
- Vite
- Tailwind CSS
- SQLite
- Tokio
- OpenTelemetry where supported
- pnpm

IMPORTANT RULES:

- Follow the master blueprint.
- Prioritize performance and low resource consumption.
- Keep the frontend and backend separated.
- Use modular provider collectors.
- Do not invent undocumented APIs.
- Verify provider documentation before implementation.
- Do not assume subscription usage is equivalent to
  token consumption.
- Do not collect prompts or source code.
- Store sensitive credentials securely.
- Avoid unnecessary dependencies.
- Do not implement Teams or Enterprise yet.

After presenting the proposed architecture,
wait for approval before implementing anything.

Once approved, implement incrementally.

The first milestone is establishing the
Tauri + React + Rust foundation and SQLite integration.

Do not proceed to the next milestone until the
current milestone has been implemented and tested.

17. Definition of Success
AgentPulse's initial release will be considered successful when a user can:
1. Install AgentPulse on Windows or macOS.
2. Open the desktop application.
3. Connect supported AI provider accounts.
4. Automatically collect available usage information.
5. View subscription usage and remaining allowances.
6. Monitor active AI coding sessions.
7. View context-window consumption.
8. Track token consumption.
9. See quota reset information when available.
10. Receive usage-limit notifications.
11. View historical usage analytics.
12. Use the application without noticeably affecting system performance.
The application must prioritize accuracy over the number of supported providers.
Final Project Direction
AgentPulse is a performance-first, local-first AI usage monitoring desktop application built with Tauri 2, Rust, React, TypeScript, and SQLite.

Our immediate focus is the Individual / Pro dashboard.

We will build the monitoring engine first, verify actual provider data collection, normalize the information, and then connect everything to the React dashboard.

The architecture must remain modular so we can introduce additional AI providers, Teams, and Enterprise capabilities in the future.

Our guiding principle: Build the monitoring engine around real provider data, not assumptions.