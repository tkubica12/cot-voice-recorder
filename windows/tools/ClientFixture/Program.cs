using VoicePrompt.App.Services;
using VoicePrompt.Core.Infrastructure;

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        try
        {
            if (args.Length == 5 && args[0] == "--client-fixture")
            {
                var fixture = SignedOutFixture.Create(args[1], args[2], args[3],
                    int.Parse(args[4], System.Globalization.CultureInfo.InvariantCulture));
                fixture.TraceProgress("validated");
                var app = new VoicePrompt.App.App(fixture);
                fixture.TraceProgress("application-constructed");
                app.InitializeComponent();
                fixture.TraceProgress("resources-initialized");
                return app.Run();
            }
            if (args.Length == 2 && args[0] == "--client-fixture-quit")
            {
                using var guard = SingleInstanceGuard.Acquire(SignedOutFixture.InstanceName(args[1]));
                if (!guard.IsPrimary)
                {
                    guard.SignalQuit();
                    if (!guard.WaitForPrimaryExit(TimeSpan.FromSeconds(10)))
                        throw new InvalidOperationException("The isolated C# fixture did not stop gracefully.");
                }
                return 0;
            }
            throw new ArgumentException("Only explicit signed-out fixture startup or isolated quit is supported.");
        }
        catch (Exception error)
        {
            Console.Error.WriteLine($"C# client fixture failed: {error.GetType().Name}: {error.Message}");
            return 1;
        }
    }
}
