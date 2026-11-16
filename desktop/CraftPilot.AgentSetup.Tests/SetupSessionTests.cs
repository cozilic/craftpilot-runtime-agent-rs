using Xunit;

namespace CraftPilot.AgentSetup.Tests;

public class SetupSessionTests
{
    [Fact]
    public void cancelling_an_operation_cancels_its_token()
    {
        var session = new SetupSession();
        var operation = session.BeginOperation(CancellationToken.None);

        Assert.False(operation.Token.IsCancellationRequested);
        Assert.True(session.Cancel());
        Assert.True(operation.Token.IsCancellationRequested);

        session.EndOperation(operation);
    }

    [Fact]
    public void cancelling_without_a_running_operation_is_a_no_op()
    {
        var session = new SetupSession();

        Assert.False(session.Cancel());
    }

    [Fact]
    public void an_operation_also_ends_when_the_caller_is_cancelled()
    {
        var session = new SetupSession();
        using var caller = new CancellationTokenSource();
        var operation = session.BeginOperation(caller.Token);

        caller.Cancel();

        Assert.True(operation.Token.IsCancellationRequested);
        session.EndOperation(operation);
    }

    [Fact]
    public void a_finished_operation_does_not_clear_the_running_one()
    {
        var session = new SetupSession();
        var first = session.BeginOperation(CancellationToken.None);
        var second = session.BeginOperation(CancellationToken.None);

        // Att den första åtgärden avslutas får inte nolla den som kör nu.
        session.EndOperation(first);

        Assert.True(session.Cancel());
        Assert.True(second.Token.IsCancellationRequested);
        session.EndOperation(second);
    }
}
