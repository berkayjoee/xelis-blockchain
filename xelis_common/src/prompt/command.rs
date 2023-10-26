use std::{collections::HashMap, pin::Pin, future::Future, fmt::Display, time::{Instant, Duration}};

use crate::config::VERSION;

use super::{argument::*, ShareablePrompt};
use anyhow::Error;
use thiserror::Error;
use log::{info, warn, error};

#[derive(Error, Debug)]
pub enum CommandError {
    #[error("Expected a command name")]
    ExpectedCommandName,
    #[error("Command was not found")]
    CommandNotFound,
    #[error("Expected required argument {}", _0)]
    ExpectedRequiredArg(String), // arg name
    #[error("Too many arguments")]
    TooManyArguments,
    #[error(transparent)]
    ArgError(#[from] ArgError),
    #[error("Invalid argument: {}", _0)]
    InvalidArgument(String),
    #[error("Exit command was called")]
    Exit,
    #[error("No data was set in command manager")]
    NoData,
    #[error("No prompt was set in command manager")]
    NoPrompt,
    #[error(transparent)]
    Any(#[from] Error)
}

pub type SyncCommandCallback<T> = fn(&CommandManager<T>, ArgumentManager) -> Result<(), CommandError>;
pub type AsyncCommandCallback<T> = fn(&'_ CommandManager<T>, ArgumentManager) -> Pin<Box<dyn Future<Output = Result<(), CommandError>> + '_>>;

pub enum CommandHandler<T> {
    Sync(SyncCommandCallback<T>),
    Async(AsyncCommandCallback<T>)
}

pub struct Command<T> {
    name: String,
    description: String,
    required_args: Vec<Arg>,
    optional_args: Vec<Arg>,
    callback: CommandHandler<T>
}

impl<T> Command<T> {
    pub fn new(name: &str, description: &str, callback: CommandHandler<T>) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            required_args: Vec::new(),
            optional_args: Vec::new(),
            callback
        }
    }

    pub fn with_optional_arguments(name: &str, description: &str, optional_args: Vec<Arg>, callback: CommandHandler<T>) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            required_args: Vec::new(),
            optional_args,
            callback
        }
    }

    pub fn with_required_arguments(name: &str, description: &str, required_args: Vec<Arg>, callback: CommandHandler<T>) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            required_args,
            optional_args: Vec::new(),
            callback
        }
    }

    pub fn with_arguments(name: &str, description: &str, required_args: Vec<Arg>, optional_args: Vec<Arg>, callback: CommandHandler<T>) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            required_args,
            optional_args,
            callback
        }
    }

    pub async fn execute(&self, manager: &CommandManager<T>, values: ArgumentManager) -> Result<(), CommandError> {
        match &self.callback {
            CommandHandler::Sync(handler) => {
                handler(manager, values)
            },
            CommandHandler::Async(handler) => {
                handler(manager, values).await
            },
        }
    }

    pub fn get_name(&self) -> &String {
        &self.name
    }

    pub fn get_description(&self) -> &String {
        &self.description
    }

    pub fn get_required_args(&self) -> &Vec<Arg> {
        &self.required_args
    }

    pub fn get_optional_args(&self) -> &Vec<Arg> {
        &self.optional_args
    }

    pub fn get_usage(&self) -> String {
        let required_args: Vec<String> = self.get_required_args()
            .iter()
            .map(|arg| format!("<{}>", arg.get_name()))
            .collect();

        let optional_args: Vec<String> = self.get_optional_args()
            .iter()
            .map(|arg| format!("[{}]", arg.get_name()))
            .collect();

        format!("{} {}{}", self.get_name(), required_args.join(" "), optional_args.join(" "))
    }
}

pub struct CommandManager<T> {
    commands: Vec<Command<T>>,
    data: Option<T>,
    prompt: Option<ShareablePrompt<T>>,
    running_since: Instant
}

impl<T> CommandManager<T> {
    pub fn new(data: Option<T>) -> Self {
        Self {
            commands: Vec::new(),
            data,
            prompt: None,
            running_since: Instant::now()
        }
    }

    pub fn default() -> Self {
        let mut zelf = CommandManager::new(None);
        zelf.add_command(Command::with_optional_arguments("help", "Show this help", vec![Arg::new("command", ArgType::String)], CommandHandler::Sync(help)));
        zelf.add_command(Command::new("version", "Show the current version", CommandHandler::Sync(version)));
        zelf.add_command(Command::new("exit", "Shutdown the daemon", CommandHandler::Sync(exit)));
        zelf
    }

    pub fn set_data(&mut self, data: Option<T>) {
        self.data = data;
    }

    pub fn get_data<'a>(&'a self) -> Result<&'a T, CommandError> {
        self.data.as_ref().ok_or(CommandError::NoData)
    }

    pub fn get_optional_data(&self) -> &Option<T> {
        &self.data
    }

    pub fn set_prompt(&mut self, prompt: Option<ShareablePrompt<T>>) {
        self.prompt = prompt;
    }

    pub fn get_prompt<'a>(&'a self) -> Result<&'a ShareablePrompt<T>, CommandError> {
        self.prompt.as_ref().ok_or(CommandError::NoPrompt)
    }

    pub fn add_command(&mut self, command: Command<T>) {
        self.commands.push(command);
    }

    pub fn get_commands(&self) -> &Vec<Command<T>> {
        &self.commands
    }

    pub fn get_command(&self, name: &str) -> Option<&Command<T>> {
        self.commands.iter().find(|command| *command.get_name() == *name)
    }

    pub async fn handle_command(&self, value: String) -> Result<(), CommandError> {
        let mut command_split = value.split_whitespace();
        let command_name = command_split.next().ok_or(CommandError::ExpectedCommandName)?;
        let command = self.get_command(command_name).ok_or(CommandError::CommandNotFound)?;
        let mut arguments: HashMap<String, ArgValue> = HashMap::new();
        for arg in command.get_required_args() {
            let arg_value = command_split.next().ok_or_else(|| CommandError::ExpectedRequiredArg(arg.get_name().to_owned()))?;
            arguments.insert(arg.get_name().clone(), arg.get_type().to_value(arg_value)?);
        }

        // include all options args available
        for optional_arg in command.get_optional_args() {
            if let Some(arg_value) = command_split.next() {
                arguments.insert(optional_arg.get_name().clone(), optional_arg.get_type().to_value(arg_value)?);
            } else {
                break;
            }
        }

        if command_split.next().is_some() {
            return Err(CommandError::TooManyArguments);
        }

        command.execute(self, ArgumentManager::new(arguments)).await
    }

    pub fn message<D: Display>(&self, message: D) {
        info!("{}", message);
    }

    pub fn warn<D: Display>(&self, message: D) {
        warn!("{}", message);
    }

    pub fn error<D: Display>(&self, message: D) {
        error!("{}", message);
    }

    pub fn running_since(&self) -> Duration {
        self.running_since.elapsed()
    }
}

fn help<T>(manager: &CommandManager<T>, mut args: ArgumentManager) -> Result<(), CommandError> {
    if args.has_argument("command") {
        let arg_value = args.get_value("command")?.to_string_value()?;
        let cmd = manager.get_command(&arg_value).ok_or(CommandError::CommandNotFound)?;
        manager.message(&format!("Usage: {}", cmd.get_usage()));
    } else {
        manager.message("Available commands:");
        for cmd in manager.get_commands() {
            manager.message(format!("- {}: {}", cmd.get_name(), cmd.get_description()));
        }
        manager.message("See how to use a command using /help <command>");
    }
    Ok(())
}

fn exit<T>(manager: &CommandManager<T>, _: ArgumentManager) -> Result<(), CommandError> {
    manager.message("Stopping...");
    Err(CommandError::Exit)
}

fn version<T>(manager: &CommandManager<T>, _: ArgumentManager) -> Result<(), CommandError> {
    manager.message(format!("Version: {}", VERSION));
    Ok(())
}