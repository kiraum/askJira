# askJira - Cody Chat API for Jira Integration

This is a Rust implementation of a Cody Chat API that interacts with the Sourcegraph API and Jira to provide context-aware responses based on Jira ticket data.

## Prerequisites

- Rust
- Cargo

## Dependencies

This project uses the following dependencies:
- tokio (1.0+)
- reqwest (0.11+)
- serde_json (1.0+)
- structopt (0.3+)
- futures (0.3+)
- env_logger
- indicatif
- log

## Environment Variables

Before running the application, make sure to set the following environment variables:

- `SRC_ACCESS_TOKEN`: Your Sourcegraph access token
- `SRC_ENDPOINT`: The Sourcegraph API endpoint URL
- `JIRA_TOKEN`: Your Jira API token
- `JIRA_HOST`: The Jira host URL

## Pre-built Binary

For macOS ARM users, a pre-built binary is available in the `bin/` directory. You can use this binary directly without building the project yourself.

To use the pre-built binary:

1. Navigate to the `bin/` directory
2. Make the binary executable: `chmod +x askJira`
3. Run the binary: `./askJira --message "Your question here"`

Make sure to set the required environment variables before running the binary.

## Installing via Homebrew

For ARM macOS users, you can install askJira using Homebrew. We provide a custom tap for easy installation:

1. Add the custom tap:
```
brew tap kiraum/askjira https://github.com/kiraum/askJira
```

2. Install askJira:
```
brew install askjira
```

This will install the latest version of askJira on your system. You can then run it directly from the command line:
```
askJira --message "Your question here"
```

Remember to set the required environment variables before running askJira.

## Building the Project

To build the project, run:
````
cargo build
````

For a release build with optimizations:
````
cargo build --release
````

## Running the Application

You can run the application using ask Cody:
````
cargo run -- --message "Your question here"

````

To include Jira context, use the `--jql` option:
````
cargo run -- --message "Your question here" --jql "project = PROJ and created > startOfMonth()"

````

Additional options:
- `--max-issues`: Maximum number of issues to fetch (default: 1000)
- `--max-results`: Maximum number of results per Jira API call (default: 100)
- `--debug`: Enable debug mode
- `--list-models`: List available models
- `--set-model`: Set the model to use

Example with all options:
```
cargo run -- --message "What are the top priority bugs?" --jql "project = PROJ AND type = Bug ORDER BY priority DESC" --max-issues 1000 --max-results 100 --debug --set-model "anthropic::2023-06-01::claude-3.5-sonnet"
```

## Features

- Fetches Jira ticket data based on provided JQL queries
- Sends chat completion requests to the Sourcegraph API
- Handles large amounts of Jira data by processing in batches
- Provides comprehensive answers based on Jira context
- Debug mode for troubleshooting
- Ability to list available models and set a specific model
- Progress indicator during processing

## Note

This client is designed to work with the Sourcegraph API and Jira API. Make sure you have the necessary permissions and valid access tokens to use this application.

This code was tested against Sourcegraph Pro/Enterprise and Jira Cloud/Server.
