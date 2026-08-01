//! Transport-independent application command execution contract.

/// Executes one application command without exposing transport details.
pub trait UseCase<Command> {
    type Output;
    type Error;

    /// Executes the command according to the use case's business rules.
    ///
    /// # Errors
    ///
    /// Returns the implementation's error when validation or execution fails.
    fn execute(&self, command: Command) -> Result<Self::Output, Self::Error>;
}
